import { ref, watch, type Ref } from 'vue'
import type { AuthStore, AuthUser } from './auth'

export type AppRoute =
  | { name: 'login' }
  | { name: 'password' }
  | { name: 'sessions' }
  | { name: 'devices' }
  | { name: 'device-detail'; params: { id: string } }
  | { name: 'tasks' }
  | { name: 'task-detail'; params: { id: string } }
  | { name: 'admin-approvals' }
  | { name: 'admin-groups' }
  | { name: 'admin-users' }
  | { name: 'admin-roles' }
  | { name: 'admin-grants' }
  | { name: 'admin-audit' }
  | { name: 'admin-system' }

export interface AppRouter {
  currentRoute: Ref<AppRoute>
  navigate(route: AppRoute): void
  resolveInitialRoute(user: AuthUser | null): AppRoute
  cleanup(): void
}

// 02 §1 & progress.md: device id is lowercase hex64; task id is lowercase standard UUIDv4
export const DEVICE_ID_HEX64 = /^[0-9a-f]{64}$/
export const TASK_ID_V4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/

function parseHash(hash: string): AppRoute {
  const path = hash.replace(/^#\/?/, '')
  if (!path || path === 'login') return { name: 'login' }
  if (path === 'password') return { name: 'password' }
  if (path === 'sessions') return { name: 'sessions' }
  if (path === 'devices') return { name: 'devices' }
  const devMatch = path.match(/^devices\/([a-zA-Z0-9_-]+)$/)
  if (devMatch) {
    const id = devMatch[1] ?? ''
    if (DEVICE_ID_HEX64.test(id)) {
      return { name: 'device-detail', params: { id } }
    }
    // Fail closed: malformed id falls back to devices
    return { name: 'devices' }
  }
  if (path === 'tasks') return { name: 'tasks' }
  const taskMatch = path.match(/^tasks\/([a-zA-Z0-9_-]+)$/)
  if (taskMatch) {
    const id = taskMatch[1] ?? ''
    if (TASK_ID_V4.test(id)) {
      return { name: 'task-detail', params: { id } }
    }
    // Fail closed: malformed id falls back to tasks
    return { name: 'tasks' }
  }
  if (path === 'admin/approvals') return { name: 'admin-approvals' }
  if (path === 'admin/groups') return { name: 'admin-groups' }
  if (path === 'admin/users') return { name: 'admin-users' }
  if (path === 'admin/roles') return { name: 'admin-roles' }
  if (path === 'admin/grants') return { name: 'admin-grants' }
  if (path === 'admin/audit') return { name: 'admin-audit' }
  if (path === 'admin/system') return { name: 'admin-system' }
  return { name: 'devices' }
}

function formatRouteToHash(route: AppRoute): string {
  switch (route.name) {
    case 'login': return '#/login'
    case 'password': return '#/password'
    case 'sessions': return '#/sessions'
    case 'devices': return '#/devices'
    case 'device-detail': return `#/devices/${route.params.id}`
    case 'tasks': return '#/tasks'
    case 'task-detail': return `#/tasks/${route.params.id}`
    case 'admin-approvals': return '#/admin/approvals'
    case 'admin-groups': return '#/admin/groups'
    case 'admin-users': return '#/admin/users'
    case 'admin-roles': return '#/admin/roles'
    case 'admin-grants': return '#/admin/grants'
    case 'admin-audit': return '#/admin/audit'
    case 'admin-system': return '#/admin/system'
  }
}

export function createRouter(auth: AuthStore): AppRouter {
  function isRestrictedAdminRoute(name: string): boolean {
    return name.startsWith('admin-')
  }

  function resolveInitialRoute(user: AuthUser | null): AppRoute {
    if (!user) return { name: 'login' }
    if (user.must_change_password) return { name: 'password' }
    return { name: 'devices' }
  }

  const currentRoute = ref<AppRoute>({ name: 'devices' })

  function guardRoute(target: AppRoute): AppRoute {
    // 编程式导航校验 canonical ID
    if (target.name === 'device-detail') {
      if (!DEVICE_ID_HEX64.test(target.params.id)) {
        return { name: 'devices' }
      }
    } else if (target.name === 'task-detail') {
      if (!TASK_ID_V4.test(target.params.id)) {
        return { name: 'tasks' }
      }
    }

    const status = auth.status.value
    const user = auth.user.value

    // 当 status 为 checking 且尚未确定 user 时，保留初始/当前路由，绝不提前冲刷改写为 login 或 devices
    if (status === 'checking' && !user) {
      return target
    }

    if (!user) {
      return { name: 'login' }
    }
    if (user.must_change_password) {
      return { name: 'password' }
    }
    // signed_in 且无需改密的用户，禁止访问 login 与 password 页面，反向守卫回 devices
    if (target.name === 'login' || target.name === 'password') {
      return { name: 'devices' }
    }
    if (isRestrictedAdminRoute(target.name) && !user.is_admin) {
      return { name: 'devices' }
    }
    return target
  }

  function navigate(target: AppRoute): void {
    const allowed = guardRoute(target)
    currentRoute.value = allowed
    const targetHash = formatRouteToHash(allowed)
    // 只有当计算出的目标 hash 与当前 window.location.hash 不一致时，才更新 window.location.hash
    // 特别是在 status === checking 时，如果当前已有 hash 且匹配 target，绝不触发多余的 hash 更新
    if (typeof window !== 'undefined' && window.location.hash !== targetHash) {
      window.location.hash = targetHash
    }
  }

  function onHashChange() {
    if (typeof window === 'undefined') return
    const routeFromHash = parseHash(window.location.hash)
    // 如果解析出的路由与 currentRoute 相同，不需要重复 navigate
    if (JSON.stringify(routeFromHash) === JSON.stringify(currentRoute.value)) {
      return
    }
    navigate(routeFromHash)
  }

  if (typeof window !== 'undefined') {
    window.addEventListener('hashchange', onHashChange)
    const initial = window.location.hash ? parseHash(window.location.hash) : resolveInitialRoute(auth.user.value)
    navigate(initial)
  }

  // 监听认证状态动态变更（例如撤权、登出、强制改密、成功登录）
  const stopAuthWatch = watch([() => auth.user.value, () => auth.status.value], ([, newStatus]) => {
    if (newStatus === 'checking') {
      return
    }
    if (newStatus === 'signed_in') {
      // 检查当前路由是否受权限守卫许可（例如非管理员访问 admin 路由或已登录用户在 login/password）
      navigate(currentRoute.value)
      return
    } else if (newStatus === 'force_password') {
      navigate({ name: 'password' })
      return
    } else if (newStatus === 'signed_out') {
      if (currentRoute.value.name !== 'login') {
        navigate({ name: 'login' })
      }
      return
    }
    navigate(currentRoute.value)
  })

  function cleanup() {
    if (typeof window !== 'undefined') {
      window.removeEventListener('hashchange', onHashChange)
    }
    stopAuthWatch()
  }

  return {
    currentRoute,
    navigate,
    resolveInitialRoute,
    cleanup,
  }
}
