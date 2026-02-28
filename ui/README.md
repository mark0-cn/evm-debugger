## Dioxus 前端迁移说明

目标：

- 前端使用 Dioxus 实现，并逐步完善功能
- 浏览器版与桌面版都通过同一套 HTTP API 交互

当前实现：

- `/`：Dioxus LiveView（通过 WebSocket `/ws` 与后端通讯）

后续可选演进：

- 如果需要 WASM/静态部署模式，再补充 `ui/dist` 构建产物并由后端提供静态资源服务。
