# EVM Debugger — Claude 开发指南

## 项目概述

基于 revm 的 EVM 交易单步调试工具。后端 Rust + axum，前端单文件 HTML。
运行后在浏览器 http://localhost:8080 调试以太坊交易。

```bash
cargo run            # 开发
cargo run --release  # 生产
```

---

## 项目结构

```
src/
  main.rs       — 入口，tokio main，监听 8080
  types.rs      — 所有共享数据结构（ChannelMessage, StepSnapshot, TraceStep 等）
  fetcher.rs    — 从 RPC 获取交易 + 写入 cache/<tx_hash>.json 缓存
  inspector.rs  — StepDebugInspector，无暂停，直接收集所有快照到 Arc<Mutex<Vec>>
  executor.rs   — 启动 EVM OS 线程，执行完毕后一次性发送所有快照
  session.rs    — DebugSession，基于索引导航已收集的快照
  server.rs     — axum 路由
static/
  index.html    — 单文件前端
cache/          — 缓存目录（自动创建）
DESIGN.md       — 完整产品设计文档
```

---

## 核心架构

EVM 跑在独立 OS 线程，**一次性跑完整笔交易**，所有快照收集好后通过 `snap_tx` 发回 session：

- `snap_tx` (SyncSender, cap=1)：EVM → HTTP，发送 `ChannelMessage::AllSnapshots`（只发一次）
- 不再有 `cmd_rx` / `DebugCommand`——步进只是在 `Vec<StepSnapshot>` 里移动索引

**加载流程：**
1. `create_session` 调用 `spawn_evm_thread`
2. EVM 线程跑完整交易，inspector 把每步快照写入 `Arc<Mutex<Vec<StepSnapshot>>>`
3. 执行结束后，线程把所有快照通过 channel 发给 HTTP handler（`ChannelMessage::AllSnapshots`）
4. HTTP handler 收到后，将快照存入 session，同时提取轻量 `Vec<TraceStep>` 返回给前端
5. 前端拿到 `trace_steps` 立即展示所有 opcode；步进时只高亮当前行

**步进响应为即时操作**（无 channel 阻塞）：
```rust
// step_into: 只移动索引
fn step_into(&self) -> SessionState {
    current_index += 1;
    return snapshots[current_index].clone()  // 从内存取，无网络/阻塞
}
```

HTTP handler 不再需要 `spawn_blocking` 处理步进命令。

---

## 已知问题与解决方案

### 1. alloy 版本冲突 → `serde::__private` 找不到

**现象：**
```
error[E0433]: failed to resolve: could not find `__private` in `serde`
  --> alloy-consensus-0.14.0/src/transaction/envelope.rs
```

**原因：** 用了旧版 alloy（0.x），与新版 serde 不兼容。revm workspace 用的是 alloy 1.4.2。

**解决：** Cargo.toml 中所有 alloy 依赖必须与 revm workspace 对齐：
```toml
alloy-provider   = { version = "1.4.2", features = ["reqwest"] }
alloy-eips       = { version = "1.4.2" }
alloy-primitives = { version = "1.5.2", features = ["serde"] }
alloy-consensus  = { version = "1.4.2" }
```

---

### 2. revm path 依赖指向 virtual workspace → 编译失败

**现象：**
```
found a virtual manifest at `/Users/lianghong/project/revm/Cargo.toml`
instead of a package manifest
```

**原因：** `/Users/lianghong/project/revm/Cargo.toml` 是 workspace manifest，不是具体 crate。

**解决：** 指向具体 crate 路径：
```toml
revm          = { path = "../revm/crates/revm", features = ["std", "serde", "optional_balance_check"] }
revm-database = { path = "../revm/crates/database", features = ["std", "alloydb"] }
revm-inspector = { path = "../revm/crates/inspector", features = ["std"] }
revm-context  = { path = "../revm/crates/context", features = ["std", "serde"] }
```

---

### 3. `rt.block_on()` 内部 channel 阻塞死锁

**现象：** 服务启动，第一次 load 交易后页面一直 loading，没有任何 snapshot 返回。

**原因：**
```rust
// 错误写法
rt.block_on(async move {
    let mut evm = ctx.build_mainnet_with_inspector(inspector);
    evm.inspect_one_tx(tx_env)  // ← 内部 inspector.step() 调用 cmd_rx.recv() 阻塞
    // block_on 的 async 执行被阻塞，tokio worker 无法处理 AlloyDB 的 DB 查询
    // → AlloyDB 永远拿不到数据 → 死锁
});
```

AlloyDB 在 `step()` 暂停期间（`cmd_rx.recv()` 阻塞时）可能需要发出新的异步 DB 查询，但整个 tokio runtime 的 executor 被同步阻塞住了。

**解决：** 用 `rt.enter()` 建立 runtime 上下文，EVM 同步执行不放进 `block_on`：
```rust
let _guard = rt.enter(); // 建立上下文，让 WrapDatabaseAsync::new() 和 block_in_place 生效

// EVM 直接同步执行，不在 block_on 里
let mut evm = ctx.build_mainnet_with_inspector(inspector);
evm.inspect_one_tx(tx_env);
// AlloyDB 遇到 DB 查询时用 block_in_place，让其他 tokio worker 继续工作
```

**原理：** `WrapDatabaseAsync` 在 multi-thread runtime 上下文里用 `tokio::task::block_in_place`（不是 `block_on`）执行 DB 查询，block_in_place 会把当前线程让出给其他 worker，所以不会死锁。

---

### 4. 重放已确认交易 → nonce / 余额检查失败 → "Execution produced no steps"

**现象：** load 交易报错 `Execution produced no steps`，服务端日志显示 `Transaction(NonceTooHigh)` 或 `Transaction(LackOfFundForMaxFee)`。

**原因：** 在 block N-1 状态重放 block N 中的第 k 笔交易（k > 0）时：
- 同一 sender 的前 k-1 笔交易已经推进了 nonce
- 前 k-1 笔交易消耗了 sender 余额用于 gas

**解决：** 在 `CfgEnv` 中同时禁用两个检查，并在 `Cargo.toml` 中开启 feature：
```toml
# Cargo.toml
revm = { path = "...", features = ["std", "serde", "optional_balance_check"] }
```
```rust
// executor.rs
c.disable_nonce_check = true;
c.disable_balance_check = true;
```

`disable_balance_check` 字段由 `#[cfg(feature = "optional_balance_check")]` 控制，必须显式开启 feature。

---

## 重要 API 参考

### revm 关键路径（基于 revm v34）

| 用途 | 路径 |
|------|------|
| Inspector trait | `revm::inspector::Inspector` |
| ContextTr | `revm::context::ContextTr` |
| JournalTr（含 depth()）| `revm::context_interface::JournalTr` |
| Transaction trait（含 gas_limit()）| `revm::context_interface::Transaction` |
| CallScheme 枚举 | `revm::interpreter::{CallScheme}` — 只有 Call/CallCode/DelegateCall/StaticCall |
| CreateInputs 字段 | 全部私有，用 `.caller()` `.value()` 方法访问 |
| ExecutionResult | `revm::context::result::ExecutionResult` — gas 字段类型是 `ResultGas`，用 `.gas.used()` 获取 gas 用量（不是 `.gas_used` 字段） |
| build_mainnet_with_inspector | `revm::MainBuilder::build_mainnet_with_inspector` |
| inspect_one_tx | `revm::inspector::InspectEvm::inspect_one_tx` |
| Stack 数据 | `interp.stack.data()` → `&[U256]`，bottom-to-top 顺序 |
| 当前 gas limit | `interp.gas.limit()`（在 initialize_interp 里读，比 tx.gas_limit() 更准确） |
| 当前合约地址 | 从 call_stack 跟踪，call() 里 `inputs.target_address` |

### HTTP API

| Method | Path | 说明 |
|--------|------|------|
| POST | `/api/session` | 创建会话，body: `{"tx_hash","rpc_url"}` |
| GET  | `/api/session/:id` | 获取当前状态 |
| POST | `/api/session/:id/step_into` | F11 |
| POST | `/api/session/:id/step_over` | F10 |
| POST | `/api/session/:id/continue` | F5 |
| POST | `/api/session/:id/abort` | 中止 |

---

## 待实现功能

- [ ] 断点支持（按 PC 或合约地址）
- [ ] 会话超时自动清理（DashMap 目前只增不减）
- [ ] 支持 EIP-4844 blob 交易
- [ ] 多交易对比视图
