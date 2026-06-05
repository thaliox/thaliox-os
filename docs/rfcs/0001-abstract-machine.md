# RFC-0001 — THALIOX Abstract Machine (TAM)

| | |
|---|---|
| **Status** | Draft |
| **Author** | THALIOX core |
| **Supersedes** | — |
| **Depends on** | [MASTER_PLAN.md](../MASTER_PLAN.md) |

> **本 RFC 定义 THALIOX 抽象机 (THALIOX Abstract Machine, TAM)——一份与具体实现无关的契约。**
> 它是编译器、运行时、以及未来协同设计的硅**共同瞄准的靶子**。
> 软件实现 (H1, 跑在 Linux) 与硬件实现 (H3, 自研芯片) 必须遵守同一份 TAM 语义,
> 这是「H1 写的东西在 H3 不被推倒重来」的保证。

---

## 1. 动机 (Motivation)

传统抽象机 (JVM、WASM、x86 ISA) 的原语是为**通用、人类导向**的计算设计的:整数、字节、指针、文本。
TAM 的赌注是:**AI 智能体的世界有三个第一性的原语**,把它们抬升为机器级公民,整个栈(从调度到安全到通信)都会因此简化:

1. **向量消息 (Vector Message)** — 智能体之间交换「意义」的单位,而非字节流。
2. **注意力预算 (Attention Budget)** — 调度与资源核算的单位,取代 CPU 时间片。
3. **能力令牌 (Capability Token)** — 权限与信任的单位,取代 uid/gid。

TAM 不规定**如何实现**这三者(软件用 struct,硅用寄存器与标签);它只规定它们的**语义与不变量 (invariants)**。

---

## 2. 抽象机模型 (Machine Model)

```
        ┌──────────────────────────────────────────────┐
        │                 TAM 世界                       │
        │                                                │
        │   Agent ── 执行单元 (相当于"进程")             │
        │     │                                          │
        │     ├─ 持有一个 AttentionBudget (它的"算力配额")│
        │     ├─ 持有若干 CapabilityToken (它"能做什么") │
        │     ├─ 通过 VectorMessage 与其他 Agent 通信    │
        │     └─ 在 SemanticSpace 中读写记忆 (取代地址空间)│
        │                                                │
        │   操作 (Operation) 都是"语义调用 (SemanticCall)"│
        │   每次操作都:                                  │
        │     · 消耗 AttentionBudget                     │
        │     · 经 CapabilityToken 鉴权                  │
        │     · 在 SemanticSpace / VectorMessage 上作用   │
        └──────────────────────────────────────────────┘
```

- **Agent**:TAM 的执行单元。每个 Agent 有唯一 `AgentId`、一个注意力预算、一组能力令牌、一份记忆视图。
- **SemanticSpace**:TAM 的"内存"。不是线性地址空间,而是**语义向量空间**——对象按含义检索,而非按地址寻址。
- **SemanticCall**:TAM 的"指令"。所有操作(发消息、读写记忆、调用工具、孵化子 agent)都是语义调用,且**三重门控**:消耗预算 + 鉴权 + 作用于状态。

### 2.1 不变量 (Invariants) — 任何实现都必须满足

- **INV-1 (预算守恒)**:任何 SemanticCall 在执行前必须从调用者的 `AttentionBudget` 扣减其声明成本;余额不足则调用被拒 (`BudgetExceeded`)。
- **INV-2 (能力前置)**:任何有副作用的 SemanticCall 必须携带一个授予所需 `Permission` 且 `scope` 覆盖目标资源的有效 `CapabilityToken`;否则被拒 (`CapabilityDenied`)。
- **INV-3 (向量保真)**:`VectorMessage` 在收发两端若共享同一 `ModelFingerprint`,其向量负载必须零损耗传递;若不共享,必须经显式翻译,且翻译的损耗可度量。
- **INV-4 (可审计)**:每个 SemanticCall 产生一条不可篡改的审计记录(谁、用什么能力、花了多少预算、作用于什么)。人类监督者可检索。
- **INV-5 (人类底线)**:存在一个**最高能力 (Sovereign Capability)**,只持于人类监督面;它可无条件暂停、快照、回滚、终止任意 Agent。任何实现不得移除此能力。

---

## 3. 原语一:向量消息 (Vector Message)

智能体间交换意义的单位。

### 3.1 逻辑结构

```rust
struct VectorMessage {
    from:        AgentId,            // 发送方
    to:          Recipient,          // 单播 / 组播(意图组)
    fingerprint: ModelFingerprint,   // 发送方向量空间标识
    kind:        MessageKind,        // Data / Intent / Translate / Control
    payload:     VectorPayload,      // 稠密/稀疏/量化向量 + 可选原始数据
    intent:      Option<IntentVector>, // 可选意图向量(语义路由用)
    seq:         u64,                // 流式分片序号
    capability:  Option<CapabilityToken>, // 跨 agent 操作所需授权
}

struct ModelFingerprint { model_id: String, revision: String, dim: u32 }

enum VectorPayload {
    Dense  { dtype: Dtype, dim: u32, data: Bytes },   // 行主序
    Sparse { dim: u32, indices: Vec<u32>, values: Bytes },
    Raw    { content_type: String, bytes: Bytes },     // 兼容逃生舱:文本/JSON
}

enum Dtype { Fp32, Fp16, Bf16, Fp8E4, Fp8E5, Int8 }
```

### 3.2 语义规则

- **同指纹零损耗 (INV-3)**:`from` 与 `to` 的 `ModelFingerprint` 相等时,接收方可直接将 `payload` 注入其模型,无需任何转换。
- **异指纹显式翻译**:不相等时,必须经向量翻译层产生新的 `payload`,并附带翻译质量度量(如余弦漂移)。**TAM 禁止隐式有损转换**。
- **Raw 逃生舱**:`VectorPayload::Raw` 允许携带文本/JSON,用于与外部世界互操作;但 TAM 视其为"未对齐",不享受零损耗保证。

### 3.3 实现映射

| 层 | 实现 |
|---|---|
| H1 软件 | `serde` 结构体,经 gRPC/QUIC 传输 |
| H2 专门化 | kernel-bypass (RDMA/io_uring),量化压缩 |
| H3 硅 | `vsend` / `vrecv` 为 ISA 指令;集合通信 (broadcast 到意图组) 为硬件原语 |

---

## 4. 原语二:注意力预算 (Attention Budget)

调度与资源核算的单位。取代 "CPU 时间片"。

### 4.1 逻辑结构

```rust
struct AttentionBudget {
    total:   u64,   // 授予的总 token 预算
    spent:   u64,   // 已消耗
    rate:    u64,   // 每秒可消耗上限 (tokens/s),用于限流
    refill:  RefillPolicy, // None / Periodic { per_sec } / OnDemand
}

impl AttentionBudget {
    fn remaining(&self) -> u64 { self.total.saturating_sub(self.spent) }
    fn charge(&mut self, cost: u64) -> Result<(), BudgetError>; // INV-1
}
```

`cost` 的计量基准是 **token**(推理 token + 检索/通信的 token 当量),因为它是 AI 工作量的自然单位。

### 4.2 调度语义

- 调度器在就绪的 Agent 中,按 **优先级 × 注意力权重 × 上下文相关性** 选择下一个获得算力的 Agent。
- **抢占**:高优先级意图可抢占低优先级 Agent 的预算配额。
- **节能/休眠**:预算耗尽或长期空闲的 Agent 被压缩为 Checkpoint(见 §6),释放资源;被唤醒时从 Checkpoint 恢复。
- **关键设计 (F10)**:调度策略本身**不是手写启发式,而是可被学习替换的策略 (LearnedPolicy)**。TAM 只规定调度器的输入(遥测向量)与输出(下一个 Agent + 配额),不规定策略如何产生。

### 4.3 实现映射

| 层 | 实现 |
|---|---|
| H1 软件 | 运行时维护预算账本;调度器是一个 Rust 服务 |
| H3 硅 | 预算是硬件寄存器;`charge` 是指令副作用;耗尽触发硬件级 trap |

---

## 5. 原语三:能力令牌 (Capability Token)

权限与信任的单位。取代 uid/gid。

### 5.1 逻辑结构

```rust
struct CapabilityToken {
    subject:     AgentId,            // 持有者
    permissions: Vec<Permission>,    // 允许的操作类别
    scope:       Vec<Scope>,         // 作用域(必须强制!)
    issued_at:   u64,
    expires_at:  u64,                // 0 = 永不过期
    jti:         [u8; 16],           // 唯一 ID,支持吊销与防重放
    delegable:   bool,               // 是否可委派给子 agent
    signature:   [u8; 32],           // 对规范化负载的 HMAC/签名
}

enum Permission { Read, Write, Execute, Spawn, Communicate, Admin, Sovereign }

struct Scope {
    resource: ResourceKind,          // Memory / Agent / Tool / Space ...
    pattern:  String,                // glob,如 "mem://team-a/*"
}
```

### 5.2 鉴权语义 (INV-2) — 必须修正历史教训

TAM 对鉴权有两条**强制规则**(直接来自对早期原型的评审):

1. **scope 必须强制**:`check(token, op, target)` 不仅校验 `permissions` 含所需类别,**还必须校验某个 `scope` 的 `pattern` 匹配 `target`**。仅校验权限类别而忽略 scope,是不符合 TAM 的(早期实现的 H1 漏洞)。
2. **签名负载必须无歧义规范化**:签名覆盖的字节必须用**长度前缀编码 (length-prefixed)** 或规范 CBOR,**禁止用分隔符拼接**(`|` / `,`),以杜绝分隔符注入导致的签名碰撞/伪造(早期实现的 H2 漏洞)。

其他规则:
- `Admin` 蕴含除 `Sovereign` 外的所有权限。
- `Sovereign` 是 INV-5 的最高能力,只发给人类监督面,**不可委派**。
- 委派:`delegable` 令牌可派生出 scope ⊆ 父 scope、过期 ≤ 父过期的子令牌;委派链可审计、可整条吊销。

### 5.3 实现映射

| 层 | 实现 |
|---|---|
| H1 软件 | HMAC-SHA256 over 规范化负载;运行时在每次 SemanticCall 前 `check` |
| H3 硅 | CHERI 式硬件能力标签:每个内存字带能力位,鉴权在硅层不可伪造地强制 |

---

## 6. 记忆与快照 (Semantic Space & Checkpoint)

- **SemanticSpace**:对象 = `{ id, vector, tags, data, capability }`;按语义向量检索,而非路径。提供 FUSE 兼容层供人类调试(可挂载为目录)。
- **记忆分层**:Working(上下文/KV-Cache)· Episodic(近期会话,带时间窗)· Semantic(长期知识,持久向量)· Procedural(技能/工具使用模式)。
- **Checkpoint**:一个 Agent 的**完整可恢复状态** = 身份 + 预算 + 能力 + 记忆指针 + 会话游标。Checkpoint 是 §4.2 休眠、以及运行时**快照/迁移/合并/自愈**的基础。
  - **迁移** = 在目标节点从 Checkpoint 重建。
  - **合并** = 两个 Checkpoint 的状态经 CRDT 无冲突合并。
  - **自愈** = 异常实例的最近 Checkpoint 在健康实例上恢复。

---

## 7. 操作集 (SemanticCall 一览)

所有操作都遵守 INV-1/2/4。最小操作集:

| 操作 | 说明 | 所需 Permission |
|---|---|---|
| `vsend` / `vrecv` | 收发向量消息 | Communicate |
| `mem.read` / `mem.search` | 读/检索记忆 | Read |
| `mem.write` / `mem.summarize` | 写/概要化记忆 | Write |
| `tool.invoke` | 调用工具(含 web_search/fetch) | Execute |
| `agent.spawn` | 孵化子 agent | Spawn |
| `cap.delegate` / `cap.revoke` | 委派/吊销能力 | (持有可委派令牌) |
| `checkpoint` / `restore` | 快照/恢复 | Admin |
| `sovereign.*` | 暂停/回滚/终止任意 agent | Sovereign(仅人类面) |

---

## 8. 与实现层的对应总表

| TAM 概念 | H1 软件 (Linux) | H3 硅 (自研) |
|---|---|---|
| Agent | microVM (Firecracker) | 硬件隔离的执行上下文 |
| VectorMessage | serde + gRPC/QUIC | `vsend`/`vrecv` ISA 指令 |
| AttentionBudget | 运行时账本 + 学习型调度器 | 硬件预算寄存器 + trap |
| CapabilityToken | HMAC + scope 强制 | CHERI 式硬件能力标签 |
| SemanticSpace | 向量数据库 (Qdrant/LanceDB) | 近存计算 + 语义寻址 |
| SemanticCall | trait 方法 | 编译器静态调度的数据流 |

---

## 9. 未决问题 (Open Questions)

1. 注意力预算的 `cost` 如何对**非推理操作**(检索、通信)统一折算为 token 当量?
2. 向量翻译的"损耗可度量"用什么标准指标(余弦漂移?下游任务保真?)?
3. CRDT 合并对"人格/记忆"这种语义状态是否足够,还是需要语义级合并策略?
4. `Sovereign` 能力的密钥托管与多签治理模型?
5. 意图组 (组播) 的成员管理与一致性?

---

## 10. 结论

TAM 把 AI 智能体世界的三个第一性原语——**向量消息、注意力预算、能力令牌**——抬升为机器级契约,并以五条不变量约束任何实现。
**这份契约是 THALIOX 从软件原型走向自研硅的脊椎:只要 H1 与 H3 都遵守 TAM,中间的演进就是替换实现,而非推倒重来。**
