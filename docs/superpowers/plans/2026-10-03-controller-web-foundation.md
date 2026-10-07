# Controller Web 基础公用层实施计划

> **致执行代理：** 使用 subagent-driven-development，逐项TDD并独立审查；使用复选框跟踪。前端基础范围已获用户批准，非整个条件性Web计划的批准。

**Goal:** 在独立中控前端交付可测试、双语、可访问的应用壳、公用组件和只读API基础。
**Architecture:** 先建立Vue/Vite/TS测试骨架，再按互斥文件并行组件、i18n、API，最后App接线；不接后端或启动服务。
**Tech Stack:** Vue3/Vite、TypeScript、Vitest、Vue Testing Library、jsdom、vue-tsc、Node22/npm，lockfile锁版本。
**Spec:** [基础设计](../specs/2026-10-03-controller-web-foundation-design.md)。

## Global Constraints

- 只写新 `apps/controller-web` 与本计划报告；不改板端ui、DSH、Rust/Cargo/DB、Makefile/CI、其他package。安装缓存也留应用目录并忽略。
- 无业务页面、session/认证/CSRF/写请求/SSE、Rust嵌入、部署或dev/preview服务。App挂载零fetch，无虚假权限/设备/服务状态。
- 所有真实凭据禁读/禁记录；合成测试不得使用secret资料。不运行真库，不扩大Task4授权。
- 缺依赖/导入/编译不是RED；先让测试可编译，再观察具体行为失败。每个实现者报告RED/GREEN和边界。
- 实现者禁止stage/commit、派代理。协调者独立审查、显式暂存，不强制加入缓存或产物。

## 文件与并行契约

F1独占工程配置、`index.html/src/main.ts/src/App.vue/src/App.test.ts/README.md/.gitignore`。
F2独占`src/components/{BaseButton,BaseInput,AppNotice,AsyncState}.vue`及各`.test.ts`、`src/styles.css`。
F3独占`src/i18n.ts/src/i18n.test.ts/src/locales/{zh-CN,en}.ts`。
F4独占`src/api.ts/src/api.test.ts/src/types.ts`。
F2/F3/F4在F1的稳定包和测试配置就绪后同时执行；测试各跑自己的文件，不能改App/main/package/lock。F5最后独占App/main/App.test接线。依赖升级只由协调者另行安排。

### F1：可验证工程骨架

**Files:** `apps/controller-web/{package.json,package-lock.json,tsconfig.json,tsconfig.node.json,vite.config.ts,index.html,.gitignore,README.md}`；`src/{main.ts,App.vue,App.test.ts,vite-env.d.ts,test-setup.ts}`。
**Produces:** `npm test -- --run`、`npm run typecheck`（vue-tsc --noEmit）、`npm run build`；jsdom渲染真实Vue SFC。App最初只静态标题/工作区main，不导入还不存在的F2/F3/F4。

- [ ] 写private package及必要工具配置，固定兼容Node22的版本；局部`.gitignore`忽略node_modules/dist/coverage/.npm-cache，安装依赖形成lock。基础配置允许先建立可运行测试环境，不把工具错误当行为RED。
- [ ] 初始App可编译为空main，再写真实DOM测试：
```ts
import {render, screen} from '@testing-library/vue';
import {expect, test} from 'vitest';
import App from './App.vue';
test('mounts a semantic foundation shell', () => {
  render(App);
  expect(screen.getByRole('main')).toBeTruthy();
  expect(screen.getByRole('heading', {level:1, name:'Rsetup Controller'})).toBeTruthy();
});
```
- [ ] `npm test -- --run src/App.test.ts` 观察标题缺失RED；实现header/main/footer及标题后GREEN。不要建立业务页面或请求健康接口。
- [ ] test/typecheck/build；README准确标未接后端、未启动服务。记录版本与安装/构建结果；不宣称浏览器视觉验收。

### F2：基础组件与样式（并行A）

**Consumes:** F1测试配置；组件接口以Spec为准，文案为props/slots，不导入i18n/api。
**Produces:** 四个真实可访问组件及CSS tokens；不修改main/App以插入展示。

- [ ] 每个组件先可编译空模板，写并逐项观察行为RED：
```ts
const view = render(BaseInput, {props:{id:'name',label:'Name',modelValue:'',error:'Required'}});
const input = screen.getByLabelText('Name');
expect(input.getAttribute('aria-invalid')).toBe('true');
expect(input.getAttribute('aria-describedby')).toContain('name-error');
await fireEvent.update(input, 'new');
expect(view.emitted()['update:modelValue']).toEqual([['new']]);
```
```ts
const view = render(BaseButton, {props:{loading:true,loadingLabel:'Working'},slots:{default:'Save'}});
const button = screen.getByRole('button');
expect(button.getAttribute('type')).toBe('button');
expect(button.hasAttribute('disabled')).toBe(true);
expect(button.getAttribute('aria-busy')).toBe('true');
await fireEvent.click(button);
expect(view.emitted().click).toBeUndefined();
```
- [ ] 实现最小组件，再补AppNotice error alert/其他status、纯文本slot、AsyncState仅error+retryLabel可手动retry，disabled/required/hint/错误解除等正反例。
- [ ] CSS实现tokens/320px适应/focus-visible/reduced-motion/contrast；不依赖伪造业务数据、图片或第三方UI库。
- [ ] `npm test -- --run src/components`、typecheck；报告行为证据和未浏览器实测边界。

### F3：双语和偏好（并行B）

**Files/Interfaces:** Spec规定createI18n，固定storage key；只写F3文件，不接App。

- [ ] 先使用能返回原key的可编译stub，真实RED验证词典与切换：
```ts
const i18n = createI18n({storage:null,document,initialLocale:'zh-CN'});
expect(i18n.t('app.notConnected')).toBe('尚未连接业务服务');
i18n.setLocale('en');
expect(i18n.t('app.notConnected')).toBe('Business services are not connected');
expect(document.documentElement.lang).toBe('en');
```
- [ ] 最小实现Vue ref、字典、偏好加载/切换，英文缺key回退中文、未知key原样。非法language/存储异常明确回退；不写除语言外的状态。
- [ ] 补真实Vue小测试组件验证响应式文案更新，输入值与fetch调用不变；存储恢复、无document、拒绝恶意HTML解释、params纯文本插值。词典key集合一致。
- [ ] `npm test -- --run src/i18n.test.ts`、typecheck；报告准确语言行为，不冒充整站业务翻译完成。

### F4：同源只读API（并行C）

**Files:** api/types/tests。`ApiError`从`api.ts`导出，包含code/messageKey/requestId?/status?/params?/retryAfter?；固定Error.message不存原始response。`ApiResponse<T>`从types导出，`get<T>`返回data/requestId。

- [ ] 可编译stub先返回未解包值，写RED：
```ts
vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({data:{revision:'18446744073709551615'},request_id:'e5f6a7b8-c9d0-4e1f-8a2b-4c5d6e7f8091'}),{status:200})));
expect(await get<{revision:string}>('/devices')).toEqual({data:{revision:'18446744073709551615'},requestId:'e5f6a7b8-c9d0-4e1f-8a2b-4c5d6e7f8091'});
```
- [ ] 最小GET实现；固定GET/credentials=same-origin/redirect=error/Accept；只有path和signal可传，App不消费API以避免真实请求。
- [ ] 路径校验RED→GREEN：`https://example.invalid/a`、`//example.invalid`、`/../other`、`/%2e%2e/other`、反斜杠、控制字符、fragment均在fetch前拒绝。合法query保留；不要让URL规范化逃出/api/v1。
- [ ] 非空request_id、data own属性，畸形JSON/object/error拒绝；401/403/404/409/429保留合法code/message_key/params/request_id，不使用服务端HTML。合成429保留Retry-After，不重试；network/abort分类。固定本地codes：INVALID_API_PATH、NETWORK_ERROR、REQUEST_ABORTED、INVALID_API_RESPONSE；messageKey为对应errors.invalidPath/network/aborted/invalidResponse。
- [ ] 只mock fetch边界，afterEach恢复；断言method/credentials/redirect/headers/signal、fetch次数无自动重试、decimalstring不转number。`npm test -- --run src/api.test.ts`、typecheck。

### F5：集成与最终前端基础验收

**Files:** App/main/App.test/README，必要时只修明确接口冲突，不重写已审模块。

- [ ] 写App集成RED：加载显示基础说明；语言切换更新accessible names和lang但输入值保留、fetch零调用；组件展示的loading禁止点击，error重试仅UI事件不发请求。
- [ ] App导入styles和已审组件/i18n，语义标题、skiplink、语言选择、四组件展示，不暗示登录/设备服务已可用。API模块留作未来调用，无挂载请求。
- [ ] `npm ci`（局部cache）、`npm test -- --run`、`npm run typecheck`、`npm run build`；已有板端 `node --test ui/*.test.mjs` 回归由主代理独立执行。不启动Vite/preview，不加Rust路由。
- [ ] 独立Spec/Quality/Security审查，检查纯文本、请求约束、未知状态、无token保存、无真实secret/diff依赖漏洞。无浏览器时明确布局/a11y手工/E2E未验，不以jsdom代替。
- [ ] 主代理记录基础完成范围并仅显式暂存源文件/lock/文档；依赖产物缓存排除，提交需检查真实秘密。真实后端联调与发布保持未完成。
