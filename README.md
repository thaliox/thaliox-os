# THALIOX — 为 AI、由 AI 打造的操作系统

> **"让 AI 重新定义 AI" — An Operating System for AI, by AI, ultimately for Humans.**

THALIOX 不是 "Linux + Agent" 的缝合体,而是**自上而下、为 AI Agent 原生设计**的操作系统:
向量取代文件、Token 流取代字节管道、注意力预算取代 CPU 时间片、能力令牌取代 uid/gid。

本仓库是从 0 重构的 **THALIOX 本体**,以 **TAM 抽象机契约**([RFC-0001](docs/rfcs/0001-abstract-machine.md))
为脊椎——确保软件实现(H1,先跑在 Linux 上验证)与未来自研硅(H3)遵守**同一份语义**,
中间的演进是"替换实现",而非"推倒重来"。

## 三条不可动摇的原则

1. **自上而下 (Top-Down)** — 先定义应用层 agent 如何工作、解决什么问题,再让运行时/内核/硬件逐层向下为它服务。硬件是 agent 世界的仆人,不是起点的枷锁。
2. **分步登月 (Staged Moonshot)** — 每个里程碑都独立有价值、可演示、可融资、可证伪下一阶段。
3. **人类是底线 (Human as the Floor)** — 可审计、可一键接管、可回滚,不可绕过(INV-5 Sovereign 能力)。

## TAM:三原语 · 五不变量

三个第一性原语(详见 [RFC-0001](docs/rfcs/0001-abstract-machine.md)):

- **向量消息 (Vector Message)** — agent 间交换"意义"的单位,而非字节流。
- **注意力预算 (Attention Budget)** — 调度与资源核算的单位(token),取代 CPU 时间片。
- **能力令牌 (Capability Token)** — 权限与信任的单位,取代 uid/gid。

五条不变量约束任何实现:**INV-1 预算守恒 · INV-2 能力前置(scope 必须强制)· INV-3 向量保真 · INV-4 可审计 · INV-5 人类底线**。

## 工作区(M1 骨架)

| crate | 层 | 职责 |
|---|---|---|
| `thaliox-core` | — | TAM 原语 + 五不变量 + SemanticCall + SemanticSpace |
| `thaliox-runtime` | L2 | agent 执行单元、生命周期、注意力调度、checkpoint |
| `thaliox-memory` | L1 | SemanticSpace + 四层记忆(working/episodic/semantic/procedural) |
| `thaliox-cognition` | L1 | 统一 LLM 接口(远程后端 + 本地离线模型) |
| `thaliox-fabric` | L3 | agent↔agent 向量传输、团队编排、CRDT 状态复制 |
| `thaliox-cap` | — | 能力令牌签发/验证(规范化**长度前缀**签名、scope 强制) |
| `thaliox-api` | L5 | 统一 API 网关 + 多语言 SDK 入口 |

## 状态

**M1 骨架(Genesis,从 0 创世)。** 类型与契约先行,逐里程碑填充实现。
路线见 [docs/MASTER_PLAN.md](docs/MASTER_PLAN.md)(H1 软件 → H2 专门化 → H3 协同设计的硅)。

## 与早期仓库的关系

`github.com/thaliox/thaliox` 是早期在 Linux 上的原型/参考实现。
本仓库 `thaliox-os` 是按 MASTER_PLAN + TAM **从 0 重构的主线**,不继承现有硬件/内核假设。

## License

Apache-2.0 OR MIT
