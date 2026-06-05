# M1 — 单机 MVP 里程碑总结

> **状态:✅ 完成 · `v0.1.0` · 2026-06-05**
> 证明的假设:**THALIOX 的编程模型成立** —— 一个单机 agent,在 TAM 五不变量约束下,能自主完成任务。

M1 是 [MASTER_PLAN](MASTER_PLAN.md) §6 H1 地平线的第一级阶梯。它的唯一职责是把
[RFC-0001 TAM 抽象机](rfcs/0001-abstract-machine.md)从纸面契约变成**可运行、可证伪**的代码,
先跑在 Linux 上,但语义不绑死 Linux——为 H2/H3 的"替换实现"留好接缝。

## 1. 交付了什么

一条端到端闭环:**给 agent 一个目标 → 模型自主决定调哪个工具 → 执行 → 结果喂回 → 再思考 → 给出答案**,
全程受注意力预算与能力令牌约束、逐笔审计。

| 能力 | crate | 说明 |
|---|---|---|
| 认知 | `cognition` | 统一 `LlmProvider::complete(messages, tools)`;Anthropic Messages / OpenAI Chat Completions(及任意兼容网关)双向渲染+解析;离线 `MockProvider` 退化 |
| 记忆 | `memory` | `SemanticSpace` 向量记忆,remember / recall |
| 工具 | `tools` | `web_search`(Tavily)/ `fetch`,实现 `Tool` 契约,带 `description()` 广播给模型 |
| 自主循环 | `runtime` | `Agent::run(goal, max_iters)`:think(广播工具)→ 模型 `tool_calls` → act(Invoke)→ 结果喂回 → 再 think,失败也喂回让模型自纠 |
| 预算 | `core` / `runtime` | 预留→真实 token `settle` 对账;失败退款 |
| 能力 | `core` / `cap` | 签名 + 过期 + scope 三重校验,act 前置 |
| 审计 | `runtime` | 每次 think / invoke 记录 op·cost·target·allowed |
| 网关 | `api` | axum HTTP:agent 生命周期 + think / remember / recall / invoke + 审计查询 |

## 2. 五不变量的落地映射

| 不变量 | M1 如何强制 |
|---|---|
| **INV-1 预算守恒** | 每次 think / invoke 先按预留扣减,执行后 `settle(reserved, actual)` 对账到真实 token;失败 `settle(reserved, 0)` 全额退款 |
| **INV-2 能力前置** | `act` 在任何副作用前校验能力令牌:签名(可插 `CapabilityVerifier`)+ 未过期 + `authorizes(perm, resource, target)` scope 强制 |
| **INV-3 向量保真** | 记忆经 `SemanticSpace` 存取,不降维成字符串键 |
| **INV-4 可审计** | 每个 `SemanticCall` 落 `AuditRecord`(op / cost / target / permission_used / allowed) |
| **INV-5 人类底线** | 能力可撤销、预算硬上限、全审计可回放;Sovereign 凌驾一切 |

## 3. 实测证据

**真实模型 glm-5.1(经 OpenAI 兼容网关)+ Tavily web_search**,目标:
> "用 web_search 查 'THALIOX AI-native operating system' 是什么,再用一句中文总结。"

模型**自主**决定调 `web_search`(并非调用方编排),执行真实搜索,把结果喂回后总结。审计轨迹:

```
✓ Think       cost=315  self            ← 模型看到工具描述,决定调 web_search
✓ ToolInvoke  cost=303  tool://web_search ← Tavily 真实搜索
✓ Think       cost=858  self            ← 据搜索结果总结
余 48524 / 50000(逐笔真实 token 对账)
```

这一步是从"被编排的工具执行"到"自主 agent"的质变:决策权在模型,约束权在 TAM。

## 4. 质量门

四门全绿(CI 铁律,见 [rust-toolchain](../README.md)):

- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace` — **30 测试**(纯函数 + 闭环 + 网关 oneshot)
- `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`

8 crate · 6 example(`single_node` / `live_node` / `tool_agent` / `secure_agent` / `autonomous_agent` / `gateway`)。

## 5. 刻意留白(M2+ 再填)

- `fabric` 仅有骨架——agent↔agent 协作、团队编排、CRDT 在 M4。
- 记忆是进程内 `InMemorySpace`;真实向量库(如 Qdrant)与持久化在后续。
- 能力签名当前可注入 `CapabilityVerifier`,生产级密钥管理待 M2。
- 单进程、无快照/恢复——这正是 **M2 microVM 化**的交付物。

## 6. 下一站:M2 microVM 化

兑现 F2/F3:一键部署 + 快照/恢复 + 自更新回滚。把 M1 这条已验证的闭环,
装进可隔离、可迁移、可回滚的运行壳里。
