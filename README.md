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
4. **清白起步 (Clean-Slate Mandate)** — 不被 x86 / 现有 CPU·GPU / Linux 内核等人类遗产框死;**目的是让 AI OS 运行效率更高、全力为 AI 服务**,而非为推翻而推翻。

## TAM:三原语 · 五不变量

三个第一性原语(详见 [RFC-0001](docs/rfcs/0001-abstract-machine.md)):

- **向量消息 (Vector Message)** — agent 间交换"意义"的单位,而非字节流。
- **注意力预算 (Attention Budget)** — 调度与资源核算的单位(token),取代 CPU 时间片。
- **能力令牌 (Capability Token)** — 权限与信任的单位,取代 uid/gid。

五条不变量约束任何实现:**INV-1 预算守恒 · INV-2 能力前置(scope 必须强制)· INV-3 向量保真 · INV-4 可审计 · INV-5 人类底线**。

## 工作区

| crate | 层 | 职责 |
|---|---|---|
| `thaliox-core` | — | TAM 原语 + 五不变量 + SemanticCall + SemanticSpace / Tool 契约 |
| `thaliox-runtime` | L2 | agent 执行单元、生命周期、注意力调度、**自主 tool-calling 循环**、审计 |
| `thaliox-memory` | L1 | SemanticSpace + 四层记忆(working/episodic/semantic/procedural) |
| `thaliox-cognition` | L1 | 统一 LLM 接口(Anthropic / OpenAI-兼容 / 本地 mock)+ tool-calling 渲染解析 |
| `thaliox-tools` | L4 | agent 可调的工具(`web_search` / `fetch`),受能力门控 |
| `thaliox-fabric` | L3 | agent↔agent 向量传输、团队编排、CRDT 状态复制(M4 起填充) |
| `thaliox-cap` | — | 能力令牌签发/验证(规范化**长度前缀**签名、scope 强制) |
| `thaliox-api` | L5 | 统一 API 网关(axum)+ 多语言 SDK 入口 |

## 状态:✅ M1 单机 MVP 完成 (2026-06-05, `v0.1.0`)

M1 验证了**编程模型成立**:一个单机 agent,在 TAM 五不变量约束下,能自主完成任务。
详见 [docs/M1-MILESTONE.md](docs/M1-MILESTONE.md)。已交付:

- **认知** — 统一 `LlmProvider`,接 Anthropic Messages / OpenAI Chat Completions(及任意兼容网关),离线 mock 退化。
- **记忆** — `SemanticSpace` 向量记忆(remember / recall)。
- **工具 + 自主闭环** — `Agent::run(goal)`:**模型自己决定调哪个工具**(`web_search` / `fetch`)、执行、把结果喂回、再思考,直到给出答案。
- **注意力预算** — 预留→真实 token 对账(INV-1 守恒),失败退款。
- **能力门控** — 每次 act 校验签名 + 过期 + scope(INV-2 前置),全程审计(INV-4)。
- **API 网关** — axum HTTP:`/agents` 生命周期 + think / remember / recall / invoke + 审计查询。

**实测(glm-5.1 + Tavily)**:给定目标,模型自主调 `web_search` → 真实搜索 → 一句话总结;
审计轨迹 `Think → ToolInvoke → Think`,预算逐笔对账。

四门全绿:`fmt` · `clippy -D warnings` · `test`(30) · `doc -D warnings`。

### 快速上手

```bash
# 自主 agent:真模型自主调工具(无 key 则脚本化 mock 演示同一闭环)
OPENAI_API_KEY=...  OPENAI_BASE_URL=...  THALIOX_MODEL=glm-5.1 \
  TAVILY_API_KEY=...  cargo run -p thaliox-runtime --example autonomous_agent

# 其它示例
cargo run -p thaliox-runtime --example single_node    # 纯离线最小回路
cargo run -p thaliox-runtime --example secure_agent   # 能力签名门控
cargo run -p thaliox-api      --example gateway        # HTTP 网关 :8088
```

## 路线

H1 软件(跑在 Linux)→ H2 专门化(向下压栈)→ H3 协同设计的硅。下一站 **M2 microVM 化**
(一键部署 + 快照/恢复 + 自更新回滚)。完整路线见 [docs/MASTER_PLAN.md](docs/MASTER_PLAN.md)。

## 与早期仓库的关系

`github.com/thaliox/thaliox` 是早期在 Linux 上的原型/参考实现(已归档)。
本仓库 `thaliox-os` 是按 MASTER_PLAN + TAM **从 0 重构的主线**,不继承现有硬件/内核假设。

## License

Apache-2.0 OR MIT
