# EVM Debugger — AGENTS.md

本文件是面向自动化代理/协作者的项目工作指南，整合自原 `CLAUDE.md` 与 `DESIGN.md`，并以当前代码实现为准。

## 项目概述

基于 `revm` 的 EVM 交易调试工具：

- 后端：Rust + axum（HTTP，默认端口 8080）
- 前端：单文件 HTML（`static/index.html`）
- 能力：输入交易哈希与 RPC URL，后端重放交易并收集每步快照，前端提供步入/步过/继续、查看 opcode/stack/memory/storage/call stack/logs
- 运行：浏览器访问 `http://localhost:8080`

## 运行方式

```bash
cargo run
cargo run --release
```

## 前置条件

- 本项目通过 path 依赖引用本地 `revm` workspace 下的具体 crates；需要在本仓库相邻路径存在 `../revm/crates/*`。
- `cache/` 为本地交易缓存目录；已在 `.gitignore` 中忽略。

## 目录结构与职责

```
src/
  main.rs       入口（tokio main），启动 HTTP 服务器
  server.rs     axum 路由与 HTTP handlers
  session.rs    DebugSession：管理已收集快照的导航（索引移动）
  executor.rs   启动 EVM OS 线程，执行交易并汇总快照
  inspector.rs  StepDebugInspector：收集每步快照（无暂停）
  fetcher.rs    从 RPC 获取交易信息，并写入 cache/<tx_hash>.json 缓存
  types.rs      共享数据结构（可序列化：ChannelMessage/SessionState/StepSnapshot/TraceStep 等）
static/
  index.html    单文件前端
cache/
  <tx_hash>.json 交易缓存（自动创建）
```

## 核心架构（以当前实现为准）

当前实现是“离线快照 + 即时步进”：

- EVM 在独立 OS 线程中一次性重放整笔交易
- Inspector 在执行过程中收集 `Vec<StepSnapshot>`（每步快照）
- 执行结束后，通过 `snap_tx` 一次性把所有快照与最终结果发回 HTTP 侧
- HTTP 侧将快照存入 `DebugSession`，并提取轻量 `Vec<TraceStep>`（用于前端 opcode 列表）
- 前端步进命令只触发服务端在 `Vec<StepSnapshot>` 中移动索引并返回对应快照，无需与 EVM 线程交互

这意味着：

- 步进是 O(1) 的内存读取（不做网络/IO）
- 创建会话会执行完整重放（成本集中在 `POST /api/session`）

## 配置与约定

- 日志：通过 `RUST_LOG` 控制（例如 `info`/`debug`），默认 `info`。
- 安全：不要在仓库中提交任何 RPC key / token；公开仓库建议使用无密钥 RPC 或在本地配置。
- 变更：小步改动、可编译可回归，每一步本地提交；对外协作用 PR。

## HTTP API

| Method | Path | 说明 |
|--------|------|------|
| POST | `/api/session` | 创建会话，body: `{"tx_hash","rpc_url"}` |
| GET  | `/api/session/:id` | 获取当前会话状态 |
| POST | `/api/session/:id/step_into` | 步入（F11） |
| POST | `/api/session/:id/step_over` | 步过（F10） |
| POST | `/api/session/:id/continue` | 运行到结束（F5） |
| POST | `/api/session/:id/abort` | 中止 |
| GET  | `/` | 返回前端 HTML |

## 前端 UI 结构（概览）

页面主要区域：

- 顶部工具栏：TX Hash、RPC URL、Load、Step Into/Over/Continue/Abort
- 中心：Opcodes 列表（高亮当前 step）
- 右侧：Stack、Memory
- 左侧：Call Stack
- 底部：Storage Changes、Logs

## revm 关键参考（常用定位）

- Inspector trait：`revm::inspector::Inspector`
- 构建 EVM：`revm::MainBuilder::build_mainnet_with_inspector`
- 执行：`revm::inspector::InspectEvm::inspect_one_tx`
- Journal depth：`revm::context_interface::JournalTr::depth()`

## 维护与演进建议（给协作者/代理）

- 先保证接口一致性：路由、前端调用路径、返回结构统一，避免“实现了但没挂路由/前端没用”的漂移。
- 业务逻辑与底层依赖解耦：RPC/provider 构造、重试策略、EVM 执行编排尽量抽为可替换模块，HTTP 层保持薄。
- 快照数据量要可控：大交易会产生大量 steps，避免每步全量 clone 大结构；优先考虑增量事件或按需聚合的模型演进。
- 变更策略：小步改动、可编译可回归，每一步本地提交。
- 如果为了方便后续 agent 更改，可以直接修改本文件，补充新的功能需求与演进约定。

## 待实现功能

- 断点支持（按 PC 或合约地址）
- 会话超时自动清理（DashMap 当前只增不减）
- EIP-4844 blob 交易支持
- 多交易对比视图
