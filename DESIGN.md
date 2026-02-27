# EVM Transaction Debugger — 产品设计文档

## 目标

基于 revm 构建一个类 VS Code 调试器体验的 EVM 交易调试工具。用户输入交易哈希后，工具自动从 RPC 获取交易（带本地缓存），然后在浏览器中提供单步调试、步入/步过、查看 opcode/stack/memory/storage/调用栈/日志等功能。

---

## 技术选型

| 组件 | 技术 |
|------|------|
| 后端 | Rust + axum HTTP 服务器 |
| EVM 引擎 | revm (本地 path 依赖) |
| RPC 接入 | alloy-provider + AlloyDB |
| 执行模式 | 真实单步：channel 暂停/恢复 EVM OS 线程 |
| 前端 | 单文件 HTML + Vanilla JavaScript |
| 缓存 | `cache/<tx_hash>.json` 本地 JSON 文件 |

---

## 项目结构

```
evm-debugger/
├── Cargo.toml
├── DESIGN.md              ← 本文件
├── src/
│   ├── main.rs            # tokio main，启动 HTTP 服务器（端口 8080）
│   ├── types.rs           # 所有共享数据结构（可序列化）
│   ├── fetcher.rs         # 从缓存或 RPC 获取交易信息
│   ├── inspector.rs       # StepDebugInspector：channel 暂停逻辑
│   ├── executor.rs        # 启动 EVM OS 线程，建立 DB 栈
│   ├── session.rs         # DebugSession：管理 channel 通信
│   └── server.rs          # axum 路由和 HTTP 处理器
└── static/
    └── index.html         # 单文件前端（HTML + JS + CSS）
```

---

## 核心架构：channel 桥接 sync EVM 和 async HTTP

```
  ┌──────────────────────────────────────────────────────┐
  │  tokio async runtime (axum HTTP server)               │
  │                                                       │
  │  POST /api/session/:id/step_into                      │
  │      ↓                                               │
  │  cmd_tx.send(StepInto)  ──────────────────────────► │──┐
  │  tokio::task::spawn_blocking(|| snap_rx.recv()) ◄── │  │
  └──────────────────────────────────────────────────────┘  │
                                                             │
  ┌──────────────────────────────────────────────────────┐  │
  │  std::thread::spawn (EVM OS 线程，阻塞型)              │  │
  │                                                       │  │
  │  Inspector::step() {                                  │  │
  │    snap_tx.send(capture_snapshot())  ◄────────────── │──┘
  │    let cmd = cmd_rx.recv()  ← 真正的暂停点             │
  │    match cmd { StepOver => set_depth_target, ... }   │
  │  }                                                    │
  └──────────────────────────────────────────────────────┘
```

**关键点：** EVM 跑在独立 OS 线程（非 tokio task），`cmd_rx.recv()` 阻塞 OS 线程；HTTP handler 用 `spawn_blocking` 等待 `snap_rx`，不阻塞 reactor。

---

## 核心数据结构 (`src/types.rs`)

```rust
// EVM 线程 → HTTP 处理器
pub enum ChannelMessage {
    Paused(Box<StepSnapshot>),
    Finished(ExecutionResultInfo),
    Error(String),
}

// HTTP 处理器 → EVM 线程
pub enum DebugCommand {
    StepInto,   // 下一条 opcode（进入 CALL）
    StepOver,   // 下一条 opcode（跳过子调用）
    Continue,   // 运行到结束
    Abort,
}

pub struct StepSnapshot {
    pub step_number: u64,
    pub pc: usize,
    pub opcode: u8,
    pub opcode_name: String,
    pub call_depth: usize,
    pub gas_remaining: u64,
    pub gas_used: u64,
    pub stack: Vec<String>,          // hex 字符串，index 0 = 栈顶
    pub memory_size: usize,
    pub memory_hex: String,
    pub storage_changes: HashMap<String, HashMap<String, String>>,
    pub call_stack: Vec<CallFrame>,
    pub logs: Vec<LogEntry>,
    pub contract_address: String,
}
```

---

## Inspector 实现 (`src/inspector.rs`)

参考：`revm/crates/inspector/src/eip3155.rs`（提取 pc/opcode/stack/memory/gas 的范式）

```rust
pub struct StepDebugInspector {
    cmd_rx:  Receiver<DebugCommand>,
    snap_tx: SyncSender<ChannelMessage>,
    step_over_target_depth: Option<usize>,  // None = StepInto 模式
    continue_mode: bool,
    call_stack: Vec<CallFrame>,
    logs: Vec<LogEntry>,
    storage_changes: HashMap<String, HashMap<String, String>>,
    step_number: u64,
    gas_initial: u64,
}
```

**`step()` 逻辑：**
1. 若 `continue_mode = true`，直接返回（不暂停）
2. 若 `step_over_target_depth = Some(D)` 且当前 depth > D，直接返回
3. 检测 SSTORE（opcode 0x55），提前捕获 key/value（执行前栈顶两个值）
4. `snap_tx.send(capture_snapshot())` → `cmd_rx.recv()` 阻塞等待命令
5. 收到命令后更新 `step_over_target_depth` / `continue_mode`

---

## EVM 线程启动 (`src/executor.rs`)

参考：`revm/examples/block_traces/src/main.rs`

```rust
std::thread::spawn(move || {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all().build().unwrap();

    rt.block_on(async move {
        let provider = ProviderBuilder::new().connect_http(rpc_url.parse()?);
        let alloy_db = AlloyDB::new(provider, BlockId::from(block_number - 1));
        let wrapped = WrapDatabaseAsync::new(alloy_db).unwrap();
        let cache_db = CacheDB::new(wrapped);

        let ctx = Context::mainnet()
            .with_db(cache_db)
            .modify_block_chained(|b| { /* 填充 block 环境 */ })
            .modify_cfg_chained(|c| { c.chain_id = chain_id; });

        let mut evm = ctx.build_mainnet_with_inspector(inspector);
        evm.inspect_one_tx(tx_env)
    })
});
```

---

## HTTP API

| Method | Path | 说明 |
|--------|------|------|
| `POST` | `/api/session` | 加载交易，创建调试会话。Body: `{"tx_hash", "rpc_url"}` |
| `GET`  | `/api/session/:id` | 获取当前状态（轮询） |
| `POST` | `/api/session/:id/step_into` | 步入（进入 CALL） |
| `POST` | `/api/session/:id/step_over` | 步过（跳过子调用） |
| `POST` | `/api/session/:id/continue` | 运行到结束 |
| `POST` | `/api/session/:id/abort` | 中止会话 |
| `GET`  | `/` | 提供前端 HTML |

---

## 前端 UI 布局 (`static/index.html`)

```
┌────────────────────────────────────────────────────────────────────┐
│  EVM Debugger   [TX Hash ________________] [RPC ________] [Load]   │
│  [Step Into F11] [Step Over F10] [Continue F5] [Abort]             │
├──────────────────────┬───────────────────────────┬─────────────────┤
│  CALL STACK          │  BYTECODE / OPCODES        │  STACK          │
│  [0] CALL 0xABCD..   │ ► [42] 0x0064 SLOAD ←PC   │ [0] 0x000..60   │
│  [1] CALL transfer   │   [43] 0x0066 PUSH1        │ [1] 0x000..40   │
│                      │   [44] 0x0068 EQ            │ ...             │
│                      │                             ├─────────────────┤
│                      │                             │  MEMORY         │
│                      │                             │ 0000: 00 00..   │
│                      │                             │ 0020: 80 00..   │
├──────────────────────┴───────────────────────────┴─────────────────┤
│  Status: Gas used=5000 remaining=21000  Depth=1  PC=0x0064  SLOAD  │
├────────────────────────────────────┬───────────────────────────────┤
│  STORAGE CHANGES                   │  LOGS                          │
│  0xABCD.. slot[0x01] → 0x1234..   │  [Transfer] from:0x.. to:0x.. │
└────────────────────────────────────┴───────────────────────────────┘
```

---

## 关键依赖版本

```toml
revm             = { path = "../revm/crates/revm", features = ["std", "serde"] }
revm-database    = { path = "../revm/crates/database", features = ["std", "alloydb"] }
revm-inspector   = { path = "../revm/crates/inspector", features = ["std"] }
revm-context     = { path = "../revm/crates/context", features = ["std", "serde"] }
alloy-provider   = { version = "1.4.2", features = ["reqwest"] }
alloy-eips       = { version = "1.4.2" }
alloy-primitives = { version = "1.5.2", features = ["serde"] }
alloy-consensus  = { version = "1.4.2" }
axum             = { version = "0.7", features = ["json"] }
tower-http       = { version = "0.5", features = ["cors"] }
tokio            = { version = "1", features = ["full"] }
dashmap          = "6"
```

---

## 运行方式

```bash
# 在 evm-debugger 目录下
cargo run

# 访问
open http://localhost:8080

# 输入 mainnet 交易哈希和 RPC URL，点击 Load
# 使用 F11/F10/F5 或按钮控制调试
```

---

## 缓存机制

- 第一次加载交易时从 RPC 拉取，保存到 `cache/<tx_hash>.json`
- 之后加载同一交易直接读取本地文件，不产生 RPC 请求
- 缓存包含：caller, gas_limit, gas_price, value, data, nonce, to, chain_id, block_env 字段

---

## 已知限制 / 未来改进

- [ ] 断点支持（设置 PC 断点，Continue 时在断点处停止）
- [ ] EIP-7702 / EOF 扩展 call scheme 支持
- [ ] 会话自动过期清理
- [ ] 多交易对比调试
- [ ] 支持本地 fork（anvil 等）
