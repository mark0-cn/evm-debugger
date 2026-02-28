## Dioxus 前端迁移说明

目标：

- 保留现有 `/`（`static/index.html`）
- 新增 `/app` 作为新前端入口（WASM），并逐步替换旧页面
- 浏览器版与桌面版都通过同一套 HTTP API 交互

后端静态资源规则：

- `/app`：返回 `ui/dist/index.html`
- `/assets/*`：映射到 `ui/dist/assets/*`
- 可通过 `EVM_DEBUGGER_APP_DIST_DIR` 覆写默认目录 `ui/dist`

建议的构建产物布局：

- `ui/dist/index.html`
- `ui/dist/assets/*`（包含 wasm/js/css 等）

当前仓库内的 `ui/dist` 仅为占位，后续由 Dioxus Web 构建产物替换即可。

