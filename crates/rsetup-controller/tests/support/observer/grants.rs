use super::{Account, Engine, ObserverError};

// This closed grammar is intentionally independent of the writer parser.
fn account_atom(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.%-:".contains(&b))
}

fn valid_account(expected: &Account) -> bool {
    account_atom(&expected.user) && account_atom(&expected.host)
}

pub(crate) fn validate_grants(
    engine: Engine,
    rows: &[Option<String>],
    expected: &Account,
) -> Result<(), ObserverError> {
    if !valid_account(expected) {
        return Err("shape_rejected");
    }
    let single = format!("'{}'@'{}'", expected.user, expected.host);
    let backtick = format!("`{}`@`{}`", expected.user, expected.host);
    let mut seen = 0u8;
    for row in rows {
        let text = row.as_deref().ok_or("shape_rejected")?;
        let (grant, account) = text.split_once(" TO ").ok_or("shape_rejected")?;
        if account != single && account != backtick {
            return Err("shape_rejected");
        }
        let bit = match (engine, grant) {
            (_, "GRANT USAGE ON *.*") => 8,
            (Engine::Mysql, "GRANT SELECT ON `performance_schema`.`data_lock_waits`") => 1,
            (Engine::Mysql, "GRANT SELECT ON `performance_schema`.`data_locks`") => 2,
            (Engine::Mysql, "GRANT SELECT ON `performance_schema`.`threads`") => 4,
            (Engine::Tidb, "GRANT PROCESS ON *.*") => 1,
            _ => return Err("shape_rejected"),
        };
        if seen & bit != 0 {
            return Err("shape_rejected");
        }
        seen |= bit;
    }
    let required = match engine {
        Engine::Mysql => 7,
        Engine::Tidb => 1,
    };
    if seen & !8 == required {
        Ok(())
    } else {
        Err("shape_rejected")
    }
}

/// Validates O against an actual writer identity, not an arbitrary supplied string.
/// `writer` must come from a successful CURRENT_USER() query and non-NULL decode;
/// callers must reject query/NULL/decode failures, never substitute a fallback.
/// O3 callers must read actual participating connections and confirm A/B share
/// this writer identity, or validate O separately against each actual writer.
/// This pure check cannot establish provenance and does not restrict writer
/// identities to O's account-atom grammar.
pub(crate) fn validate_identity(
    engine: Engine,
    current: &str,
    writer: &str,
    expected: &Account,
    current_role: Option<&str>,
    mandatory_roles: Option<&str>,
) -> Result<(), ObserverError> {
    if !valid_account(expected) {
        return Err("shape_rejected");
    }
    if writer.is_empty()
        || current != format!("{}@{}", expected.user, expected.host)
        || current == writer
    {
        return Err("identity_mismatch");
    }
    // TiDB has no mandatory_roles query; external injection is a separate authorization gate.
    if current_role != Some("NONE") || (engine == Engine::Mysql && mandatory_roles != Some("")) {
        return Err("roles_unverified");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::{Account, Engine, validate_grants, validate_identity};

    fn account() -> Account {
        Account {
            user: "o_fixture".into(),
            host: "%".into(),
        }
    }
    fn mysql() -> Vec<Option<String>> {
        ["data_lock_waits", "data_locks", "threads"]
            .into_iter()
            .map(|t| {
                Some(format!(
                    "GRANT SELECT ON `performance_schema`.`{t}` TO 'o_fixture'@'%'"
                ))
            })
            .collect()
    }
    fn tidb() -> Vec<Option<String>> {
        vec![Some("GRANT PROCESS ON *.* TO 'o_fixture'@'%'".into())]
    }

    #[test]
    fn optional_usage_quotes_and_every_row_order_are_accepted() {
        fn permutations(rows: &mut [Option<String>], start: usize, engine: Engine) {
            if start == rows.len() {
                assert_eq!(validate_grants(engine, rows, &account()), Ok(()));
            } else {
                for i in start..rows.len() {
                    rows.swap(start, i);
                    permutations(rows, start + 1, engine);
                    rows.swap(start, i);
                }
            }
        }
        for (engine, base) in [(Engine::Mysql, mysql()), (Engine::Tidb, tidb())] {
            for quote in ['\'', '`'] {
                for usage in [false, true] {
                    let mut rows = base.clone();
                    if usage {
                        rows.push(Some("GRANT USAGE ON *.* TO 'o_fixture'@'%'".into()));
                    }
                    for row in &mut rows {
                        *row = row.as_ref().map(|s| s.replace('\'', &quote.to_string()));
                    }
                    permutations(&mut rows, 0, engine);
                }
            }
        }
    }

    #[test]
    fn incomplete_duplicate_null_and_cross_engine_sets_are_rejected() {
        for (engine, base, other) in [
            (Engine::Mysql, mysql(), tidb()),
            (Engine::Tidb, tidb(), mysql()),
        ] {
            for rows in [
                vec![],
                vec![None],
                other,
                vec![Some("GRANT USAGE ON *.* TO 'o_fixture'@'%'".into())],
            ] {
                assert_eq!(
                    validate_grants(engine, &rows, &account()),
                    Err("shape_rejected")
                );
            }
            for i in 0..base.len() {
                let mut missing = base.clone();
                missing.remove(i);
                assert_eq!(
                    validate_grants(engine, &missing, &account()),
                    Err("shape_rejected")
                );
                let mut duplicate = base.clone();
                duplicate.push(base[i].clone());
                assert_eq!(
                    validate_grants(engine, &duplicate, &account()),
                    Err("shape_rejected")
                );
                let mut null = base.clone();
                null[i] = None;
                assert_eq!(
                    validate_grants(engine, &null, &account()),
                    Err("shape_rejected")
                );
            }
            let mut duplicate_usage = base;
            duplicate_usage.extend(vec![
                Some("GRANT USAGE ON *.* TO 'o_fixture'@'%'".into());
                2
            ]);
            assert_eq!(
                validate_grants(engine, &duplicate_usage, &account()),
                Err("shape_rejected")
            );
        }
    }

    #[test]
    fn extra_privileges_and_unknown_statements_are_rejected() {
        for grant in [
            "GRANT SELECT ON `performance_schema`.`extra`",
            "GRANT SELECT ON `performance_schema`.*",
            "GRANT SELECT ON *.*",
            "GRANT SELECT (`id`) ON `performance_schema`.`threads`",
            "GRANT SELECT, INSERT ON `performance_schema`.`threads`",
            "GRANT SELECT ON `mysql`.`tidb`",
            "GRANT SUPER ON *.*",
            "GRANT ALL PRIVILEGES ON `business`.*",
            "GRANT PROCESS ON `business`.*",
            "GRANT SYSTEM_USER ON *.*",
            "GRANT 'role_x'@'%'",
            "GRANT PROXY ON 'writer'@'%'",
            "REVOKE SELECT ON `performance_schema`.`threads`",
            "GRANT SELECT ON performance_schema.threads",
            "GRANT SELECT ON `Performance_schema`.`threads`",
        ] {
            for (engine, mut rows) in [(Engine::Mysql, mysql()), (Engine::Tidb, tidb())] {
                rows.push(Some(format!("{grant} TO 'o_fixture'@'%'")));
                assert_eq!(
                    validate_grants(engine, &rows, &account()),
                    Err("shape_rejected")
                );
                rows[0] = rows.pop().unwrap();
                assert_eq!(
                    validate_grants(engine, &rows, &account()),
                    Err("shape_rejected")
                );
            }
        }
    }

    #[test]
    fn exact_serialization_rejects_suffixes_comments_whitespace_and_truncation() {
        for (engine, base) in [(Engine::Mysql, mysql()), (Engine::Tidb, tidb())] {
            for i in 0..base.len() {
                let text = base[i].as_ref().unwrap();
                let mut mutations: Vec<String> = [
                    " WITH GRANT OPTION",
                    " WITH ADMIN OPTION",
                    "; SELECT 1",
                    ";",
                    " -- x",
                    " # x",
                    " /* x */",
                    " ",
                    "\n",
                    "\r",
                    "\t",
                    "\0",
                ]
                .into_iter()
                .map(|s| format!("{text}{s}"))
                .collect();
                mutations.extend([
                    format!(" {text}"),
                    format!("/* x */{text}"),
                    text.to_lowercase(),
                    text.replace("GRANT ", "GRANT  "),
                    text.replace(" ON ", "\tON "),
                    text.replace(" TO ", " /* x */ TO "),
                ]);
                mutations.extend((0..text.len()).map(|end| text[..end].to_owned()));
                for mutation in mutations {
                    let mut rows = base.clone();
                    rows[i] = Some(mutation);
                    assert_eq!(
                        validate_grants(engine, &rows, &account()),
                        Err("shape_rejected")
                    );
                }
            }
        }
    }

    #[test]
    fn account_quotes_and_atoms_are_closed_and_exact() {
        for bad in [
            "'other'@'%'",
            "'o_fixture'@'localhost'",
            "'O_fixture'@'%'",
            "'o_fixture'@`%`",
            "`o_fixture`@'%'",
            "\"o_fixture\"@\"%\"",
            "o_fixture@%",
            "''@'%'",
            "'o_fixture'@''",
            "'o_fixture'@'%",
            "'o_fixture'@'%'@'x'",
            "'o_fixture\\'x'@'%'",
            "'o_fixture''x'@'%'",
            "'o_fixture' @ '%'",
            "'o_fixture'@'é'",
        ] {
            for (engine, mut rows) in [(Engine::Mysql, mysql()), (Engine::Tidb, tidb())] {
                rows[0] = rows[0].as_ref().map(|s| s.replace("'o_fixture'@'%'", bad));
                assert_eq!(
                    validate_grants(engine, &rows, &account()),
                    Err("shape_rejected")
                );
            }
        }
        for atom in [
            "", "a@b", "a b", "a\\b", "a'b", "a`b", "a\"b", "a\nb", "a\0b", "é",
        ] {
            for expected in [
                Account {
                    user: atom.into(),
                    host: "%".into(),
                },
                Account {
                    user: "o_fixture".into(),
                    host: atom.into(),
                },
            ] {
                for engine in [Engine::Mysql, Engine::Tidb] {
                    assert_eq!(
                        validate_grants(engine, &tidb(), &expected),
                        Err("shape_rejected")
                    );
                    assert_eq!(
                        validate_identity(
                            engine,
                            &format!("{}@{}", expected.user, expected.host),
                            "writer@%",
                            &expected,
                            Some("NONE"),
                            Some("")
                        ),
                        Err("shape_rejected")
                    );
                }
            }
        }
        let expected = Account {
            user: "AZaz09_.%-:".into(),
            host: "AZaz09_.%-:".into(),
        };
        for quote in ['\'', '`'] {
            let rows = vec![Some(format!(
                "GRANT PROCESS ON *.* TO {quote}{}{quote}@{quote}{}{quote}",
                expected.user, expected.host
            ))];
            assert_eq!(validate_grants(Engine::Tidb, &rows, &expected), Ok(()));
        }
    }

    #[test]
    fn identity_rejects_empty_writer_mysql() {
        assert_eq!(
            validate_identity(
                Engine::Mysql,
                "o_fixture@%",
                "",
                &account(),
                Some("NONE"),
                Some("")
            ),
            Err("identity_mismatch")
        );
    }

    #[test]
    fn identity_rejects_empty_writer_tidb() {
        assert_eq!(
            validate_identity(
                Engine::Tidb,
                "o_fixture@%",
                "",
                &account(),
                Some("NONE"),
                Some("")
            ),
            Err("identity_mismatch")
        );
    }

    #[test]
    fn identity_and_roles_fail_with_fixed_independent_classes() {
        for engine in [Engine::Mysql, Engine::Tidb] {
            for current in [
                "",
                "writer@%",
                "O_fixture@%",
                "o_fixture@localhost",
                "o_fixture@% ",
                "o_fixture@%@x",
            ] {
                assert_eq!(
                    validate_identity(
                        engine,
                        current,
                        "writer@%",
                        &account(),
                        Some("NONE"),
                        Some("")
                    ),
                    Err("identity_mismatch")
                );
            }
            assert_eq!(
                validate_identity(
                    engine,
                    "o_fixture@%",
                    "o_fixture@%",
                    &account(),
                    Some("NONE"),
                    Some("")
                ),
                Err("identity_mismatch")
            );
            for role in [
                None,
                Some(""),
                Some("none"),
                Some("NONE "),
                Some("role_x"),
                Some("`NONE`"),
                Some("NONE,role_x"),
            ] {
                assert_eq!(
                    validate_identity(
                        engine,
                        "o_fixture@%",
                        "writer@%",
                        &account(),
                        role,
                        Some("")
                    ),
                    Err("roles_unverified")
                );
            }
        }
        for mandatory in [None, Some("forced"), Some("NONE"), Some(" ")] {
            assert_eq!(
                validate_identity(
                    Engine::Mysql,
                    "o_fixture@%",
                    "writer@%",
                    &account(),
                    Some("NONE"),
                    mandatory
                ),
                Err("roles_unverified")
            );
            assert_eq!(
                validate_identity(
                    Engine::Tidb,
                    "o_fixture@%",
                    "writer@%",
                    &account(),
                    Some("NONE"),
                    mandatory
                ),
                Ok(())
            );
        }
    }

    #[test]
    fn shared_closed_grammar_accepts_exact_sets() {
        assert_eq!(validate_grants(Engine::Mysql, &mysql(), &account()), Ok(()));
        assert_eq!(validate_grants(Engine::Tidb, &tidb(), &account()), Ok(()));
    }
    #[test]
    fn shared_identity_accepts_independent_account_without_roles() {
        for engine in [Engine::Mysql, Engine::Tidb] {
            assert_eq!(
                validate_identity(
                    engine,
                    "o_fixture@%",
                    "writer@%",
                    &account(),
                    Some("NONE"),
                    Some("")
                ),
                Ok(())
            );
        }
    }
}
