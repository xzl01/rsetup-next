use super::*;
use std::time::Instant;

impl<R: IdentityRepository> AuthService<R> {
    pub async fn list_sessions(
        &self,
        actor: &Session,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<SessionPage, ControllerError> {
        // 1. actor must be active and not must_change_password
        if !actor.user.active {
            return Err(ControllerError::InvalidArgument);
        }
        if actor.user.must_change_password {
            return Err(ControllerError::PermissionDenied);
        }

        // 2. Validate limit 1..=200
        if !(1..=200).contains(&limit) {
            return Err(ControllerError::InvalidArgument);
        }

        // 3. Service checks actor.digest itself live before processing
        let now = Instant::now();
        if !self.clock.is_live(&actor.digest, now) {
            return Err(ControllerError::InvalidArgument);
        }

        // 4. Cursor validation if present:
        // Cursor is 132 lowercase hex = alias(32B, 64 hex) + limit(2B, 4 hex) + mac(32B, 64 hex).
        let cursor_after_alias = if let Some(cursor_str) = cursor {
            if cursor_str.len() != 132 {
                return Err(ControllerError::InvalidArgument);
            }
            if !cursor_str
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            {
                return Err(ControllerError::InvalidArgument);
            }
            let alias_hex = &cursor_str[0..64];
            let limit_hex = &cursor_str[64..68];
            let mac_hex = &cursor_str[68..132];

            let mut alias_bytes = [0u8; 32];
            if hex::decode_to_slice(alias_hex, &mut alias_bytes).is_err() {
                return Err(ControllerError::InvalidArgument);
            }
            let mut limit_bytes = [0u8; 2];
            if hex::decode_to_slice(limit_hex, &mut limit_bytes).is_err() {
                return Err(ControllerError::InvalidArgument);
            }
            let cursor_limit = u16::from_be_bytes(limit_bytes) as usize;
            if cursor_limit != limit {
                return Err(ControllerError::InvalidArgument);
            }

            let mut expected_mac_payload = Vec::with_capacity(16 + 16 + 32 + 2);
            expected_mac_payload.extend_from_slice(&actor.user.id);
            expected_mac_payload.extend_from_slice(&self.clock.epoch);
            expected_mac_payload.extend_from_slice(&alias_bytes);
            expected_mac_payload.extend_from_slice(&limit_bytes);

            let expected_mac = self.alias_key.sign_cursor(&expected_mac_payload);
            let mut provided_mac = [0u8; 32];
            if hex::decode_to_slice(mac_hex, &mut provided_mac).is_err() {
                return Err(ControllerError::InvalidArgument);
            }

            // constant time compare MAC
            let mut diff = 0u8;
            for i in 0..32 {
                diff |= expected_mac[i] ^ provided_mac[i];
            }
            if diff != 0 {
                return Err(ControllerError::InvalidArgument);
            }

            Some((alias_hex.to_string(), alias_bytes))
        } else {
            None
        };

        // 5. Read sessions from repository
        let rows = self
            .repo
            .list_user_sessions(actor, self.clock.epoch)
            .await?;

        // 6. Hard limit capacity at 8192 (8193 -> ResourceExhausted)
        if rows.len() > 8192 {
            return Err(ControllerError::ResourceExhausted);
        }

        // 7. Validate each row: owner_id must match actor, process_epoch must match clock.epoch, not polluted
        struct Candidate {
            alias: String,
            current: bool,
            created_time: chrono::DateTime<chrono::Utc>,
        }
        let mut candidates = Vec::with_capacity(rows.len());

        for row in rows {
            if row.owner_id != actor.user.id || row.process_epoch != self.clock.epoch {
                return Err(ControllerError::Config("polluted session row".into()));
            }
            if row.revoked {
                continue;
            }
            if !self.clock.is_live(&row.digest, now) {
                continue;
            }
            let alias = self.alias_key.alias(&row.digest);
            let current = row.digest == actor.digest;
            candidates.push(Candidate {
                alias,
                current,
                created_time: row.created_time,
            });
        }

        // 8. Sort by public alias (lexicographical)
        candidates.sort_by(|a, b| a.alias.cmp(&b.alias));

        // 9. Apply cursor filtering
        let after_index = if let Some((ref start_alias, _)) = cursor_after_alias {
            match candidates.iter().position(|c| &c.alias == start_alias) {
                Some(pos) => pos + 1,
                None => {
                    // Start alias not found among current live items; find insertion point
                    candidates
                        .iter()
                        .position(|c| &c.alias > start_alias)
                        .unwrap_or(candidates.len())
                }
            }
        } else {
            0
        };

        let page_items: Vec<_> = candidates
            .into_iter()
            .skip(after_index)
            .take(limit + 1)
            .collect();
        let has_more = page_items.len() > limit;
        let returned_candidates = if has_more {
            &page_items[..limit]
        } else {
            &page_items[..]
        };

        let next_cursor = if has_more {
            let last_item = returned_candidates
                .last()
                .expect("must have last item if has_more");
            let mut alias_bytes = [0u8; 32];
            hex::decode_to_slice(&last_item.alias, &mut alias_bytes)
                .map_err(|_| ControllerError::Crypto)?;
            let limit_u16 = limit as u16;
            let limit_bytes = limit_u16.to_be_bytes();

            let mut mac_payload = Vec::with_capacity(16 + 16 + 32 + 2);
            mac_payload.extend_from_slice(&actor.user.id);
            mac_payload.extend_from_slice(&self.clock.epoch);
            mac_payload.extend_from_slice(&alias_bytes);
            mac_payload.extend_from_slice(&limit_bytes);

            let mac = self.alias_key.sign_cursor(&mac_payload);
            let cursor_str = format!(
                "{}{}{}",
                last_item.alias,
                hex::encode(limit_bytes),
                hex::encode(mac)
            );
            Some(cursor_str)
        } else {
            None
        };

        let items = returned_candidates
            .iter()
            .map(|c| SessionItem {
                id: c.alias.clone(),
                current: c.current,
                created_time: c
                    .created_time
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            })
            .collect();

        Ok(SessionPage { items, next_cursor })
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests_support::auth_fixture;
    use super::*;
    use crate::auth::session::IDLE;

    #[tokio::test]
    async fn list_sessions_two_sessions_and_current_marker() {
        let (svc, _repo) = auth_fixture();
        let _login1 = svc.login("alice", "old password").await.unwrap();
        let login2 = svc.login("alice", "old password").await.unwrap();

        let page = svc.list_sessions(&login2.session, None, 50).await.unwrap();
        assert_eq!(page.items.len(), 2);
        let cur = page
            .items
            .iter()
            .find(|i| i.current)
            .expect("must have current session");
        let other = page
            .items
            .iter()
            .find(|i| !i.current)
            .expect("must have other session");
        assert_ne!(cur.id, other.id);
        assert_eq!(cur.id.len(), 64);
        assert_eq!(other.id.len(), 64);
        assert!(page.next_cursor.is_none());
    }

    #[tokio::test]
    async fn list_sessions_does_not_contain_other_user_sessions() {
        let (svc, repo) = auth_fixture();
        let login_alice = svc.login("alice", "old password").await.unwrap();

        // inject a session for user [2; 16] into FakeRepo
        let other_digest = token_digest(b"other-user-raw-token");
        let now = chrono::Utc::now();
        repo.state
            .lock()
            .unwrap()
            .1
            .push((other_digest, false, svc.clock.epoch, [2; 16], now));

        let page = svc
            .list_sessions(&login_alice.session, None, 50)
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert!(page.items[0].current);
    }

    #[tokio::test]
    async fn list_sessions_idle_expiry_without_renewal() {
        let (svc, _repo) = auth_fixture();
        let login1 = svc.login("alice", "old password").await.unwrap();
        let login2 = svc.login("alice", "old password").await.unwrap();

        // Move login1's clock back so it is 100 milliseconds before 30min idle boundary
        let now = Instant::now();
        svc.clock.insert(
            login1.session.digest,
            now - IDLE + std::time::Duration::from_millis(100),
        );

        let page = svc.list_sessions(&login2.session, None, 50).await.unwrap();
        assert_eq!(
            page.items.len(),
            2,
            "session before 30min idle boundary must be present"
        );

        // Now move login1 to exactly 30min idle boundary
        let now2 = Instant::now();
        svc.clock.insert(login1.session.digest, now2 - IDLE);
        let page2 = svc.list_sessions(&login2.session, None, 50).await.unwrap();
        assert_eq!(
            page2.items.len(),
            1,
            "session at or past 30min idle boundary must be absent"
        );
        assert!(page2.items[0].current);
    }

    #[tokio::test]
    async fn list_sessions_limits_and_pagination() {
        let (svc, _repo) = auth_fixture();
        let login1 = svc.login("alice", "old password").await.unwrap();
        let _login2 = svc.login("alice", "old password").await.unwrap();

        // limit 0 is invalid
        assert!(matches!(
            svc.list_sessions(&login1.session, None, 0).await,
            Err(ControllerError::InvalidArgument)
        ));
        // limit 201 is invalid (> 200)
        assert!(matches!(
            svc.list_sessions(&login1.session, None, 201).await,
            Err(ControllerError::InvalidArgument)
        ));
        // limit 1 on 2 items returns 1 item and a 132-hex cursor
        let page1 = svc.list_sessions(&login1.session, None, 1).await.unwrap();
        assert_eq!(page1.items.len(), 1);
        let cursor = page1.next_cursor.expect("should have next_cursor");
        assert_eq!(cursor.len(), 132);

        // Fetch second page using cursor
        let page2 = svc
            .list_sessions(&login1.session, Some(&cursor), 1)
            .await
            .unwrap();
        assert_eq!(page2.items.len(), 1);
        assert!(page2.next_cursor.is_none());
        assert_ne!(page1.items[0].id, page2.items[0].id);
        // items should be strictly ordered by alias
        assert!(page1.items[0].id < page2.items[0].id);

        // Fetch with limit 200
        let page_200 = svc.list_sessions(&login1.session, None, 200).await.unwrap();
        assert_eq!(page_200.items.len(), 2);
        assert!(page_200.next_cursor.is_none());

        // Fetch with default limit 50
        let page_50 = svc.list_sessions(&login1.session, None, 50).await.unwrap();
        assert_eq!(page_50.items.len(), 2);
        assert!(page_50.next_cursor.is_none());
    }

    #[tokio::test]
    async fn list_sessions_corrupted_foreign_or_tampered_cursor_is_invalid_argument() {
        let (svc, _repo) = auth_fixture();
        let login = svc.login("alice", "old password").await.unwrap();
        let _login2 = svc.login("alice", "old password").await.unwrap();

        let page = svc.list_sessions(&login.session, None, 1).await.unwrap();
        let cursor = page.next_cursor.unwrap();

        // Wrong length
        assert!(matches!(
            svc.list_sessions(&login.session, Some(&cursor[..130]), 1)
                .await,
            Err(ControllerError::InvalidArgument)
        ));
        // Uppercase hex
        let upper_hex = cursor.to_ascii_uppercase();
        assert!(matches!(
            svc.list_sessions(&login.session, Some(&upper_hex), 1).await,
            Err(ControllerError::InvalidArgument)
        ));
        // Non-hex
        let mut non_hex = cursor.clone();
        non_hex.replace_range(0..2, "zz");
        assert!(matches!(
            svc.list_sessions(&login.session, Some(&non_hex), 1).await,
            Err(ControllerError::InvalidArgument)
        ));
        // Tampered MAC
        let mut tampered_mac = cursor.clone();
        let last_char = if tampered_mac.ends_with('0') {
            '1'
        } else {
            '0'
        };
        tampered_mac.pop();
        tampered_mac.push(last_char);
        assert!(matches!(
            svc.list_sessions(&login.session, Some(&tampered_mac), 1)
                .await,
            Err(ControllerError::InvalidArgument)
        ));
        // Limit mismatch: cursor was minted with limit=1, pass limit=2
        assert!(matches!(
            svc.list_sessions(&login.session, Some(&cursor), 2).await,
            Err(ControllerError::InvalidArgument)
        ));
        // Foreign user
        let mut foreign_actor = login.session.clone();
        foreign_actor.user.id = [9; 16];
        assert!(matches!(
            svc.list_sessions(&foreign_actor, Some(&cursor), 1).await,
            Err(ControllerError::InvalidArgument)
        ));
    }

    #[tokio::test]
    async fn list_sessions_capacity_8192_passes_8193_resource_exhausted() {
        let (svc, repo) = auth_fixture();
        let login = svc.login("alice", "old password").await.unwrap();

        // Exactly 8192 rows (bounded at 8192) passes without ResourceExhausted
        let now = chrono::Utc::now();
        let mut sessions_8192 = Vec::with_capacity(8192);
        for i in 0..8192u32 {
            let mut d = [0u8; 32];
            d[..4].copy_from_slice(&i.to_be_bytes());
            sessions_8192.push((d, false, svc.clock.epoch, login.session.user.id, now));
        }
        repo.state.lock().unwrap().1 = sessions_8192;

        let result = svc.list_sessions(&login.session, None, 50).await;
        // Alice's login session digest is not in the synthetic loop, so candidates may be empty or filtered by clock, but it must NOT be ResourceExhausted!
        assert!(result.is_ok());

        // 8193 rows -> ResourceExhausted
        let mut sessions_8193 = Vec::with_capacity(8193);
        for i in 0..8193u32 {
            let mut d = [0u8; 32];
            d[..4].copy_from_slice(&i.to_be_bytes());
            sessions_8193.push((d, false, svc.clock.epoch, login.session.user.id, now));
        }
        repo.state.lock().unwrap().1 = sessions_8193;

        let result = svc.list_sessions(&login.session, None, 50).await;
        assert!(matches!(result, Err(ControllerError::ResourceExhausted)));
    }

    #[tokio::test]
    async fn list_sessions_rejects_inactive_actor_session_digest() {
        let (svc, _repo) = auth_fixture();
        let login = svc.login("alice", "old password").await.unwrap();

        // Expire the actor's session digest in clock
        let now = Instant::now();
        svc.clock.insert(login.session.digest, now - IDLE);

        let result = svc.list_sessions(&login.session, None, 50).await;
        assert!(matches!(result, Err(ControllerError::InvalidArgument)));
    }

    #[tokio::test]
    async fn list_sessions_rejects_must_change_password_and_inactive() {
        let (svc, _repo) = auth_fixture();
        let login = svc.login("alice", "old password").await.unwrap();

        // must_change_password
        let mut force_pw_session = login.session.clone();
        force_pw_session.user.must_change_password = true;
        assert!(matches!(
            svc.list_sessions(&force_pw_session, None, 50).await,
            Err(ControllerError::PermissionDenied)
        ));

        // inactive
        let mut inactive_session = login.session.clone();
        inactive_session.user.active = false;
        assert!(matches!(
            svc.list_sessions(&inactive_session, None, 50).await,
            Err(ControllerError::InvalidArgument)
        ));
    }

    #[tokio::test]
    async fn list_sessions_rejects_polluted_repo_row() {
        struct PollutedRepo {
            polluted_rows: Vec<StoredSession>,
        }
        impl IdentityRepository for PollutedRepo {
            async fn find_user(
                &self,
                _username: &str,
            ) -> Result<Option<IdentityUser>, ControllerError> {
                unimplemented!()
            }
            async fn insert_session(
                &self,
                _user: &IdentityUser,
                _digest: [u8; 32],
                _epoch: [u8; 16],
            ) -> Result<(), ControllerError> {
                unimplemented!()
            }
            async fn find_session(
                &self,
                _digest: [u8; 32],
                _epoch: [u8; 16],
            ) -> Result<Option<IdentityUser>, ControllerError> {
                unimplemented!()
            }
            async fn change_password(
                &self,
                _session: &Session,
                _epoch: [u8; 16],
                _next_hash: &str,
            ) -> Result<(), ControllerError> {
                unimplemented!()
            }
            async fn revoke_session(
                &self,
                _session: &Session,
                _epoch: [u8; 16],
            ) -> Result<(), ControllerError> {
                unimplemented!()
            }
            async fn record_login_failure(
                &self,
                _user_id: Option<[u8; 16]>,
            ) -> Result<(), ControllerError> {
                unimplemented!()
            }
            async fn list_user_sessions(
                &self,
                _actor: &Session,
                _epoch: [u8; 16],
            ) -> Result<Vec<StoredSession>, ControllerError> {
                Ok(self.polluted_rows.clone())
            }
        }

        let (svc_orig, _repo) = auth_fixture();
        let login = svc_orig.login("alice", "old password").await.unwrap();
        let now = chrono::Utc::now();

        // 1. Polluted epoch
        let repo_bad_epoch = Arc::new(PollutedRepo {
            polluted_rows: vec![StoredSession {
                digest: login.session.digest,
                owner_id: login.session.user.id,
                process_epoch: [0xee; 16], // wrong epoch
                created_time: now,
                revoked: false,
            }],
        });
        let svc_bad_epoch = AuthService {
            repo: repo_bad_epoch,
            clock: svc_orig.clock,
            alias_key: svc_orig.alias_key,
            hasher: PasswordHasher::new(),
            dummy_hash: svc_orig.dummy_hash.clone(),
        };
        let res = svc_bad_epoch.list_sessions(&login.session, None, 50).await;
        assert!(
            matches!(res, Err(ControllerError::Config(ref msg)) if msg == "polluted session row")
        );

        // 2. Polluted owner
        let repo_bad_owner = Arc::new(PollutedRepo {
            polluted_rows: vec![StoredSession {
                digest: login.session.digest,
                owner_id: [0xff; 16], // wrong owner
                process_epoch: svc_bad_epoch.clock.epoch,
                created_time: now,
                revoked: false,
            }],
        });
        let svc_bad_owner = AuthService {
            repo: repo_bad_owner,
            clock: svc_bad_epoch.clock,
            alias_key: svc_bad_epoch.alias_key,
            hasher: PasswordHasher::new(),
            dummy_hash: svc_bad_epoch.dummy_hash,
        };
        let res2 = svc_bad_owner.list_sessions(&login.session, None, 50).await;
        assert!(
            matches!(res2, Err(ControllerError::Config(ref msg)) if msg == "polluted session row")
        );
    }
}
