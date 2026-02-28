# EVM Debugger

基于 `revm` 的 EVM 交易调试工具：输入交易哈希与 RPC URL，后端重放交易并收集每步快照，前端提供步入/步过/继续与多视图查看（opcode/stack/memory/storage/call stack/logs）。

## 功能特性

- 交易回放：按链上区块环境重放已上链交易并生成逐步快照
- 即时步进：步入/步过/继续均为本地快照索引移动（无需再次 RPC）
- 快照缓存：首次回放后落盘 trace cache，再次加载同一交易可跳过回放阶段 RPC
- 轻量 opcode 列表：服务端返回 `trace_steps`，前端可立即渲染全量 opcode 列表
- 异步创建会话：创建会话会先返回 `Loading`，前端轮询等待回放完成（避免大交易阻塞连接）

## 快速开始

前置条件：

- Rust toolchain（edition 2021）
- 可访问的以太坊 JSON-RPC（主网/对应网络）

启动服务：

```bash
cargo run
```

浏览器打开：

```
http://localhost:8080
```

在页面顶部输入：

- TX Hash：交易哈希（支持大小写与是否带 `0x`，服务端会规范化）
- RPC URL：JSON-RPC 入口（建议使用带额度/稳定的 RPC）

然后点击 Load（或在 TX Hash 输入框按 Enter）。

## 缓存说明

本项目会在 `cache/` 目录写入本地缓存（已在 `.gitignore` 忽略）：

- `cache/<hash>.json`：交易基础信息缓存（由后端 fetcher 拉取并保存）
- `cache/trace_<chain>_<block>_<hash>.json`：完整执行快照缓存（首次回放后落盘）

命中 trace cache 时，创建会话将直接从本地快照恢复，不再触发回放阶段的 RPC 状态读取。

缓存清理：

- `EVM_DEBUGGER_CACHE_TTL_SECS` 控制 `cache/` 清理阈值（秒），默认 7 天。

## 代理（可选）

如果你的环境需要代理访问外网 RPC，可在运行前设置环境变量：

```bash
export https_proxy=http://127.0.0.1:7890
export http_proxy=http://127.0.0.1:7890
export all_proxy=socks5://127.0.0.1:7890
```

不同工具/库对大小写支持不一致，必要时可同时设置 `HTTPS_PROXY/HTTP_PROXY/ALL_PROXY`。

## HTTP API

后端基于 axum，默认监听 `:8080`：

| Method | Path | 说明 |
|--------|------|------|
| POST | `/api/session` | 创建会话，body: `{"tx_hash","rpc_url"}` |
| GET  | `/api/session/:id` | 获取当前会话状态 |
| GET  | `/api/session/:id/trace_steps` | 获取全量 opcode 列表（就绪后可用） |
| POST | `/api/session/:id/step_into` | 步入（F11） |
| POST | `/api/session/:id/step_over` | 步过（F10） |
| POST | `/api/session/:id/continue` | 运行到结束（F5） |
| POST | `/api/session/:id/abort` | 中止 |
| GET  | `/` | 返回前端 HTML |

## 安全注意事项

- 不要把任何 RPC key/token 写入仓库或日志输出。
- 公网免费 RPC 可能会限流（429）。建议使用付费/稳定 RPC。

## 代码结构

核心模块分工见 [AGENTS.md](./AGENTS.md)。
