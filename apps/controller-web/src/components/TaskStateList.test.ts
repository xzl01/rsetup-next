import { fireEvent, render, screen } from '@testing-library/vue'
import { describe, expect, it, vi } from 'vitest'
import TaskStateList from './TaskStateList.vue'
import { createI18n } from '../i18n'
import type { SubTaskItem } from '../task-ops'

const i18n = createI18n({ initialLocale: 'zh-CN', storage: null })
const DEV_A = 'a'.repeat(64)
const SUB_ID = '123e4567-e89b-42d3-a456-426614174000'

function sub(overrides: Partial<SubTaskItem> = {}): SubTaskItem {
  return {
    id: SUB_ID, device_id: DEV_A, state: 'unknown',
    reason_code: 'RESULT_TIMEOUT', revision: '4', lock_released: false,
    result_evidence: 'boot_transition_observed', ...overrides,
  }
}

type ListProps = {
  subtasks: SubTaskItem[]
  showReleaseAction?: boolean
  emptyKey?: string
  onReleaseLock?: (subtask: SubTaskItem) => void
}

function renderList(props: ListProps) {
  return render(TaskStateList, { props, global: { provide: { i18n } } })
}

describe('TaskStateList truthful state presentation', () => {
  it('renders the localized state label, reason code label, revision and result evidence kind', () => {
    renderList({ subtasks: [sub()] })
    const row = screen.getByTestId(`subtask-row-${SUB_ID}`)
    expect(row.textContent).toContain(i18n.t('task.state.unknown'))
    expect(row.textContent).toContain(i18n.t('task.reason.RESULT_TIMEOUT'))
    expect(row.textContent).toContain('4')
    // 只展示当前授权的结果证据种类（snake_case 字面值），不冒称完整 wire 证据对象/成功证明
    expect(row.textContent).toContain('boot_transition_observed')
    expect(row.textContent).toContain(i18n.t('task.detail.resultEvidence'))
  })

  it('hides the evidence line when result_evidence is null and does not leak a token digest', () => {
    // result_evidence 为 null（证据未产生）：不渲染证据行；扩展字段 execution_token/其摘要不被模板消费——
    // 用真实传入组件的受控 synthetic 扩展对象（synthetic 值不来自真实秘密；DTO 类型不新增这两个字段，守卫忽略不拒绝），
    // 证明整个模板不 JSON 序列化/debug dump 整个对象（RED 阶段：空桩 getByTestId 先行抛错，真实未来 RED）
    const incoming = {
      ...sub({ result_evidence: null }),
      execution_token: 'synthetic-never-render-token-7f3a91',
      execution_token_hash: 'synthetic-never-render-hash-c4d802',
    }
    renderList({ subtasks: [incoming] })
    const row = screen.getByTestId(`subtask-row-${SUB_ID}`)
    expect(row.textContent).not.toContain(i18n.t('task.detail.resultEvidence'))
    expect(row.textContent).not.toContain('boot_transition_observed')
    // 组件输出（含行外）不得出现两个唯一 synthetic 值：模板未把整个对象序列化/dump
    expect(document.body.textContent).not.toContain('synthetic-never-render-token-7f3a91')
    expect(document.body.textContent).not.toContain('synthetic-never-render-hash-c4d802')
  })

  it('separates the ten states without collapsing accepted into success or unknown into failure', () => {
    const states = ['queued', 'held', 'dispatching', 'accepted', 'verifying',
      'succeeded', 'failed', 'unknown', 'cancelled', 'expired'] as const
    renderList({ subtasks: states.map((state, index) => sub({
      id: `sub-${index}`, state, reason_code: null, lock_released: true,
    })) })
    for (const state of states) {
      expect(screen.getAllByText(i18n.t(`task.state.${state}`)).length).toBeGreaterThan(0)
    }
    const acceptedRow = screen.getByTestId('subtask-row-sub-3')
    expect(acceptedRow.textContent).not.toContain(i18n.t('task.state.succeeded'))
    const unknownRow = screen.getByTestId('subtask-row-sub-7')
    expect(unknownRow.textContent).not.toContain(i18n.t('task.state.failed'))
    expect(unknownRow.textContent).not.toContain(i18n.t('state.retry'))
  })

  it('shows the release action only when authorized, unknown and not already released', async () => {
    const releaseLock = vi.fn()
    const { rerender } = renderList({
      subtasks: [sub()], showReleaseAction: false, onReleaseLock: releaseLock,
    })
    expect(screen.queryByTestId(`release-lock-${SUB_ID}`)).toBeNull()

    await rerender({ subtasks: [sub()], showReleaseAction: true, onReleaseLock: releaseLock })
    const button = screen.getByTestId(`release-lock-${SUB_ID}`)
    await fireEvent.click(button)
    expect(releaseLock).toHaveBeenCalledWith(sub())

    await rerender({ subtasks: [sub({ lock_released: true })], showReleaseAction: true, onReleaseLock: releaseLock })
    expect(screen.queryByTestId(`release-lock-${SUB_ID}`)).toBeNull()

    await rerender({ subtasks: [sub({ state: 'succeeded' })], showReleaseAction: true, onReleaseLock: releaseLock })
    expect(screen.queryByTestId(`release-lock-${SUB_ID}`)).toBeNull()
  })

  it('renders the empty notice without any hidden-target count', () => {
    renderList({ subtasks: [] })
    expect(screen.getByText(i18n.t('task.list.empty'))).toBeTruthy()
    expect(document.body.textContent).not.toMatch(/\d+\s*(个|hidden|隐藏)/)
  })
})
