//! The **Agent** — the live execution unit that turns the TAM contract into a
//! running thing. Every [`act`](Agent::act) is triple-gated:
//!
//! 1. **INV-2** — capability check (permission **and** scope), skipped only for
//!    budget-only [`Think`](thaliox_core::Operation::Think);
//! 2. **INV-1** — reserve the declared cost *before* executing, then **reconcile
//!    to the actual cost** afterwards (a `Think`'s real token usage; a failed
//!    call refunds the reservation);
//! 3. act on state (cognition / memory);
//! 4. **INV-4** — emit an [`AuditRecord`].
//!
//! INV-2 in full: when a [`CapabilityVerifier`] is configured
//! (see [`with_verifier`](Agent::with_verifier)), each candidate
//! capability is checked for an **authentic signature** and a **live expiry**
//! *before* its permission + scope. Without one, capabilities are trusted as
//! granted (the M1 default).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;
use thaliox_cognition::{Completion, LlmProvider, Message, ToolSpec};
use thaliox_core::{
    AgentId, AttentionBudget, AuditRecord, CapabilityToken, CapabilityVerifier, Operation,
    ResourceKind, SemanticObject, SemanticSpace, TamError, Tool,
};

use crate::Phase;

/// A parameterized, executable agent action. Each carries its **declared cost**
/// (tokens), charged before execution per INV-1.
// `Remember` inlines a `SemanticObject`; `Action` is a short-lived command
// (not bulk-stored), so the inter-variant size gap is harmless here.
#[allow(clippy::large_enum_variant)]
pub enum Action {
    /// Introspect via cognition. Budget-only (no capability).
    Think { prompt: String, cost: u64 },
    /// Write an object into the agent's memory. Needs `Write` over the target.
    Remember { object: SemanticObject, cost: u64 },
    /// Recall the `k` nearest objects. Needs `Read` over the agent's space.
    Recall {
        query: Vec<f32>,
        k: usize,
        cost: u64,
    },
    /// Invoke a tool (web_search / fetch / …) by name. Needs `Execute` over
    /// `tool://<name>`.
    Invoke {
        tool: String,
        input: String,
        cost: u64,
    },
}

impl Action {
    fn operation(&self) -> Operation {
        match self {
            Action::Think { .. } => Operation::Think,
            Action::Remember { .. } => Operation::MemWrite,
            Action::Recall { .. } => Operation::MemSearch,
            Action::Invoke { .. } => Operation::ToolInvoke,
        }
    }

    fn declared_cost(&self) -> u64 {
        match self {
            Action::Think { cost, .. }
            | Action::Remember { cost, .. }
            | Action::Recall { cost, .. }
            | Action::Invoke { cost, .. } => *cost,
        }
    }

    /// The resource kind an action acts on (for INV-2 scope matching).
    fn resource(&self) -> ResourceKind {
        match self {
            Action::Invoke { .. } => ResourceKind::Tool,
            _ => ResourceKind::Memory,
        }
    }
}

/// The result of an [`Action`].
#[derive(Debug)]
pub enum Outcome {
    /// A completion from cognition.
    Thought(Completion),
    /// The id of the object remembered.
    Remembered(String),
    /// Objects recalled by semantic similarity.
    Recalled(Vec<SemanticObject>),
    /// A tool's textual output.
    Invoked(String),
}

/// A live agent: identity + attention budget + capabilities + a memory view +
/// a cognition backend, plus its lifecycle phase and audit log.
pub struct Agent {
    id: AgentId,
    budget: AttentionBudget,
    caps: Vec<CapabilityToken>,
    memory: Arc<dyn SemanticSpace>,
    mind: Arc<dyn LlmProvider>,
    tools: HashMap<String, Arc<dyn Tool>>,
    verifier: Option<Arc<dyn CapabilityVerifier>>,
    phase: Phase,
    audit: Vec<AuditRecord>,
}

impl Agent {
    /// Spawn an agent (`Born`) with a budget, a memory view, and a mind.
    pub fn new(
        id: AgentId,
        budget: AttentionBudget,
        memory: Arc<dyn SemanticSpace>,
        mind: Arc<dyn LlmProvider>,
    ) -> Self {
        Self {
            id,
            budget,
            caps: Vec::new(),
            memory,
            mind,
            tools: HashMap::new(),
            verifier: None,
            phase: Phase::Born,
            audit: Vec::new(),
        }
    }

    /// Grant a capability to the agent (builder-style).
    pub fn grant(mut self, cap: CapabilityToken) -> Self {
        self.caps.push(cap);
        self
    }

    /// Register a tool the agent may invoke (keyed by [`Tool::name`]).
    pub fn with_tool(mut self, tool: Arc<dyn Tool>) -> Self {
        self.tools.insert(tool.name().to_string(), tool);
        self
    }

    /// Verify capability **signatures and expiry** on every `act` (INV-2).
    /// Without a verifier, capabilities are trusted as granted (the M1 default).
    pub fn with_verifier(mut self, verifier: Arc<dyn CapabilityVerifier>) -> Self {
        self.verifier = Some(verifier);
        self
    }

    /// Activate the agent: `Born → Live`.
    pub fn start(&mut self) -> Result<(), TamError> {
        if !self.phase.can_transition_to(Phase::Live) {
            return Err(TamError::Invalid(format!(
                "cannot start agent {} from {:?}",
                self.id, self.phase
            )));
        }
        self.phase = Phase::Live;
        Ok(())
    }

    /// The agent's identity.
    pub fn id(&self) -> &AgentId {
        &self.id
    }

    /// Current lifecycle phase.
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Tokens of attention budget remaining.
    pub fn remaining_budget(&self) -> u64 {
        self.budget.remaining()
    }

    /// The immutable audit trail (INV-4).
    pub fn audit(&self) -> &[AuditRecord] {
        &self.audit
    }

    /// The resource target an action acts on (for scope matching), in the
    /// agent's namespace.
    fn target_of(&self, action: &Action) -> String {
        match action {
            Action::Think { .. } => "self".to_string(),
            Action::Remember { object, .. } => format!("mem://{}/{}", self.id, object.id),
            Action::Recall { .. } => format!("mem://{}/", self.id),
            Action::Invoke { tool, .. } => format!("tool://{tool}"),
        }
    }

    /// Execute an action under the full TAM gate (see the module docs).
    pub async fn act(&mut self, action: Action) -> Result<Outcome, TamError> {
        if self.phase != Phase::Live {
            return Err(TamError::Invalid(format!(
                "agent {} is not live ({:?})",
                self.id, self.phase
            )));
        }

        let op = action.operation();
        let cost = action.declared_cost();
        let perm = op.required_permission();
        let resource = action.resource();
        let target = self.target_of(&action);
        let now = now_millis();
        let now_secs = now / 1000;

        // INV-2: a usable capability must be **authentic** (signature + unexpired,
        // when a verifier is configured) AND grant the permission over the target.
        if let Some(p) = perm {
            let authorized = self.caps.iter().any(|c| {
                self.verifier.as_ref().is_none_or(|v| v.verify(c, now_secs))
                    && c.authorizes(p, resource, &target)
            });
            if !authorized {
                self.record(op, perm, 0, &target, now, false);
                return Err(TamError::CapabilityDenied(format!("{p:?} on {target}")));
            }
        }

        // INV-1: charge the declared cost before doing the work.
        if let Err(e) = self.budget.charge(cost) {
            self.record(op, perm, 0, &target, now, false);
            return Err(e);
        }

        // Act on state; capture the *actual* cost for reconciliation.
        let acted: Result<(Outcome, u64), TamError> = match action {
            Action::Think { prompt, .. } => self
                .mind
                .complete(&[Message::user(prompt)], &[])
                .await
                .map(|c| {
                    let actual = c.tokens;
                    (Outcome::Thought(c), actual)
                }),
            Action::Remember { object, .. } => {
                let id = object.id.clone();
                self.memory
                    .put(object)
                    .map(|()| (Outcome::Remembered(id), cost))
            }
            Action::Recall { query, k, .. } => self
                .memory
                .search(&query, k)
                .map(|hits| (Outcome::Recalled(hits), cost)),
            Action::Invoke { tool, input, .. } => match self.tools.get(&tool) {
                Some(t) => t
                    .invoke(&input)
                    .await
                    .map(|r| (Outcome::Invoked(r.output), r.cost)),
                None => Err(TamError::NotFound(format!("tool '{tool}'"))),
            },
        };

        match acted {
            Ok((outcome, actual)) => {
                // INV-1 reconciliation: settle the reservation to the real cost.
                self.budget.settle(cost, actual);
                // INV-4: audit records the *actual* cost.
                self.record(op, perm, actual, &target, now, true);
                Ok(outcome)
            }
            Err(e) => {
                // Execution failed → refund the reservation; audit the failure.
                self.budget.settle(cost, 0);
                self.record(op, perm, 0, &target, now, false);
                Err(e)
            }
        }
    }

    /// **Autonomous loop** — give the agent a goal and let cognition decide which
    /// tools to call. Each think step is budget-gated (INV-1); each tool call
    /// goes through the full `act` gate (INV-1/2/4). The agent's registered tools
    /// are advertised automatically; the loop ends when the model answers in
    /// text or `max_iters` is reached.
    pub async fn run(
        &mut self,
        goal: impl Into<String>,
        max_iters: u32,
    ) -> Result<String, TamError> {
        if self.phase != Phase::Live {
            return Err(TamError::Invalid(format!("agent {} is not live", self.id)));
        }
        let specs = self.tool_specs();
        let mut convo = vec![Message::user(goal)];

        for _ in 0..max_iters {
            let completion = self.think_with_tools(&convo, &specs).await?;
            if completion.tool_calls.is_empty() {
                return Ok(completion.content); // final text answer
            }
            // Replay the assistant turn (with its tool_use), then run each call.
            convo.push(Message::assistant_with_tools(
                completion.content.clone(),
                completion.tool_calls.clone(),
            ));
            for tc in &completion.tool_calls {
                let result = match self
                    .act(Action::Invoke {
                        tool: tc.name.clone(),
                        input: tool_input(&tc.arguments),
                        cost: 200,
                    })
                    .await
                {
                    Ok(Outcome::Invoked(out)) => out,
                    Ok(_) => "(unexpected outcome)".to_string(),
                    // Feed the error back so the model can self-correct.
                    Err(e) => format!("error: {e}"),
                };
                convo.push(Message::tool_result(tc.id.clone(), result));
            }
        }
        Ok("[reached max iterations without a final answer]".to_string())
    }

    /// A budget-gated cognition step advertising `specs`. Like `Think`, it is not
    /// capability-gated (introspection), but it reconciles the real token cost
    /// and is audited (INV-1 / INV-4).
    async fn think_with_tools(
        &mut self,
        convo: &[Message],
        specs: &[ToolSpec],
    ) -> Result<Completion, TamError> {
        const RESERVE: u64 = 500;
        let now = now_millis();
        self.budget.charge(RESERVE)?;
        match self.mind.complete(convo, specs).await {
            Ok(c) => {
                self.budget.settle(RESERVE, c.tokens);
                self.record(Operation::Think, None, c.tokens, "self", now, true);
                Ok(c)
            }
            Err(e) => {
                self.budget.settle(RESERVE, 0);
                self.record(Operation::Think, None, 0, "self", now, false);
                Err(e)
            }
        }
    }

    /// Advertise the agent's registered tools to the model (a unified single
    /// `input` string parameter per tool).
    fn tool_specs(&self) -> Vec<ToolSpec> {
        self.tools
            .values()
            .map(|t| ToolSpec {
                name: t.name().to_string(),
                description: t.description().to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": { "input": { "type": "string", "description": "the tool input" } },
                    "required": ["input"]
                }),
            })
            .collect()
    }

    fn record(
        &mut self,
        op: Operation,
        perm: Option<thaliox_core::Permission>,
        cost: u64,
        target: &str,
        at: u64,
        allowed: bool,
    ) {
        self.audit.push(AuditRecord {
            agent: self.id.clone(),
            op,
            permission_used: perm,
            cost,
            target: target.to_string(),
            at,
            allowed,
        });
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Extract a tool's single `input` argument from the model's JSON arguments,
/// falling back to the first string value, then the raw arguments.
fn tool_input(arguments: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(arguments).unwrap_or(serde_json::Value::Null);
    if let Some(s) = v.get("input").and_then(|x| x.as_str()) {
        return s.to_string();
    }
    if let Some(obj) = v.as_object() {
        for val in obj.values() {
            if let Some(s) = val.as_str() {
                return s.to_string();
            }
        }
    }
    arguments.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use thaliox_cognition::MockProvider;
    use thaliox_core::{Permission, Scope};
    use thaliox_memory::InMemorySpace;

    fn cap(subject: &str, perm: Permission, pattern: &str) -> CapabilityToken {
        CapabilityToken {
            subject: AgentId::new(subject),
            permissions: vec![perm],
            scope: vec![Scope {
                resource: ResourceKind::Memory,
                pattern: pattern.into(),
            }],
            issued_at: 0,
            expires_at: 0,
            jti: [0; 16],
            delegable: false,
            signature: [0; 32],
        }
    }

    fn obj(id: &str, v: Vec<f32>) -> SemanticObject {
        SemanticObject {
            id: id.into(),
            vector: v,
            tags: vec![],
            data: vec![],
            capability: None,
        }
    }

    fn agent_with(budget: u64, caps: Vec<CapabilityToken>) -> Agent {
        let mut a = Agent::new(
            AgentId::new("a1"),
            AttentionBudget::new(budget, 1000),
            Arc::new(InMemorySpace::new()),
            Arc::new(MockProvider::new("ok", 5)),
        );
        for c in caps {
            a = a.grant(c);
        }
        a.start().unwrap();
        a
    }

    #[tokio::test]
    async fn think_is_budget_only_and_charges() {
        let mut a = agent_with(100, vec![]);
        // No capability granted, yet Think works (INV-2 skips it) and spends budget.
        let out = a
            .act(Action::Think {
                prompt: "hi".into(),
                cost: 5,
            })
            .await
            .unwrap();
        assert!(matches!(out, Outcome::Thought(_)));
        assert_eq!(a.remaining_budget(), 95);
        assert_eq!(a.audit().len(), 1);
        assert!(a.audit()[0].allowed);
        assert_eq!(a.audit()[0].permission_used, None);
    }

    #[tokio::test]
    async fn remember_requires_write_in_scope() {
        // No capability → denied, budget untouched, audit records the denial.
        let mut a = agent_with(100, vec![]);
        let err = a
            .act(Action::Remember {
                object: obj("n1", vec![1.0, 0.0]),
                cost: 3,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, TamError::CapabilityDenied(_)));
        assert_eq!(a.remaining_budget(), 100);
        assert!(!a.audit()[0].allowed);

        // Grant Write over the agent's namespace → it succeeds.
        let mut a = agent_with(100, vec![cap("a1", Permission::Write, "mem://a1/*")]);
        let out = a
            .act(Action::Remember {
                object: obj("n1", vec![1.0, 0.0]),
                cost: 3,
            })
            .await
            .unwrap();
        assert!(matches!(out, Outcome::Remembered(_)));
        assert_eq!(a.remaining_budget(), 97);
    }

    #[tokio::test]
    async fn out_of_scope_is_denied_even_with_permission() {
        // Write, but scoped to notes/* only.
        let mut a = agent_with(100, vec![cap("a1", Permission::Write, "mem://a1/notes/*")]);
        let err = a
            .act(Action::Remember {
                object: obj("secret", vec![1.0]),
                cost: 3,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, TamError::CapabilityDenied(_)));
    }

    #[tokio::test]
    async fn budget_exhaustion_rejects_before_acting() {
        let mut a = agent_with(8, vec![]);
        a.act(Action::Think {
            prompt: "1".into(),
            cost: 5,
        })
        .await
        .unwrap();
        // Only 3 left; a cost-5 think is rejected and does not run.
        let err = a
            .act(Action::Think {
                prompt: "2".into(),
                cost: 5,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, TamError::BudgetExceeded { need: 5, have: 3 }));
        assert_eq!(a.remaining_budget(), 3);
    }

    #[tokio::test]
    async fn not_live_cannot_act() {
        let mut a = Agent::new(
            AgentId::new("a1"),
            AttentionBudget::new(100, 1000),
            Arc::new(InMemorySpace::new()),
            Arc::new(MockProvider::new("ok", 5)),
        );
        // Never started (still Born).
        assert!(
            a.act(Action::Think {
                prompt: "x".into(),
                cost: 1
            })
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn think_reconciles_to_actual_token_cost() {
        // The mock really spends 20 tokens though the action declared only 5;
        // after reconciliation the budget and the audit reflect the real 20.
        let mut a = Agent::new(
            AgentId::new("a1"),
            AttentionBudget::new(100, 1000),
            Arc::new(InMemorySpace::new()),
            Arc::new(MockProvider::new("done", 20)),
        );
        a.start().unwrap();
        let out = a
            .act(Action::Think {
                prompt: "x".into(),
                cost: 5,
            })
            .await
            .unwrap();
        assert!(matches!(out, Outcome::Thought(_)));
        assert_eq!(a.remaining_budget(), 80); // reserved 5 → settled to 20
        assert_eq!(a.audit()[0].cost, 20); // audit records the real cost
    }

    /// A provider that always fails — to test that a failed call refunds its
    /// reservation.
    struct FailProvider;

    #[async_trait::async_trait]
    impl LlmProvider for FailProvider {
        fn id(&self) -> &str {
            "fail"
        }
        fn is_local(&self) -> bool {
            true
        }
        async fn complete(&self, _: &[Message], _: &[ToolSpec]) -> Result<Completion, TamError> {
            Err(TamError::Provider("boom".into()))
        }
    }

    #[tokio::test]
    async fn failed_think_refunds_reservation() {
        let mut a = Agent::new(
            AgentId::new("a1"),
            AttentionBudget::new(100, 1000),
            Arc::new(InMemorySpace::new()),
            Arc::new(FailProvider),
        );
        a.start().unwrap();
        let err = a
            .act(Action::Think {
                prompt: "x".into(),
                cost: 5,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, TamError::Provider(_)));
        assert_eq!(a.remaining_budget(), 100); // reservation refunded
        assert!(!a.audit()[0].allowed);
    }

    use thaliox_core::ToolResult;

    struct EchoTool;

    #[async_trait::async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }
        async fn invoke(&self, input: &str) -> Result<ToolResult, TamError> {
            Ok(ToolResult {
                output: format!("echo:{input}"),
                cost: 3,
            })
        }
    }

    fn tool_cap() -> CapabilityToken {
        CapabilityToken {
            subject: AgentId::new("a1"),
            permissions: vec![Permission::Execute],
            scope: vec![Scope {
                resource: ResourceKind::Tool,
                pattern: "tool://*".into(),
            }],
            issued_at: 0,
            expires_at: 0,
            jti: [0; 16],
            delegable: false,
            signature: [0; 32],
        }
    }

    fn agent_with_tool() -> Agent {
        Agent::new(
            AgentId::new("a1"),
            AttentionBudget::new(100, 1000),
            Arc::new(InMemorySpace::new()),
            Arc::new(MockProvider::new("ok", 5)),
        )
        .with_tool(Arc::new(EchoTool))
    }

    #[tokio::test]
    async fn tool_invoke_needs_execute_in_scope() {
        // No capability → denied.
        let mut a = agent_with_tool();
        a.start().unwrap();
        let err = a
            .act(Action::Invoke {
                tool: "echo".into(),
                input: "hi".into(),
                cost: 2,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, TamError::CapabilityDenied(_)));

        // Grant Execute over tool://* → it runs; cost reconciles to the tool's 3.
        let mut a = agent_with_tool().grant(tool_cap());
        a.start().unwrap();
        let out = a
            .act(Action::Invoke {
                tool: "echo".into(),
                input: "hi".into(),
                cost: 2,
            })
            .await
            .unwrap();
        assert!(matches!(out, Outcome::Invoked(s) if s == "echo:hi"));
        assert_eq!(a.remaining_budget(), 97); // reserved 2 → settled 3
    }

    #[tokio::test]
    async fn unknown_tool_is_not_found() {
        let mut a = agent_with_tool().grant(tool_cap());
        a.start().unwrap();
        let err = a
            .act(Action::Invoke {
                tool: "nope".into(),
                input: String::new(),
                cost: 1,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, TamError::NotFound(_)));
    }

    #[tokio::test]
    async fn forged_capability_rejected_when_verifying() {
        use thaliox_cap::HmacSigner;
        let signer = Arc::new(HmacSigner::new(b"issuer-key".to_vec()));

        // A forged token: right permission + scope, but a bogus signature.
        let forged = cap("a1", Permission::Write, "mem://a1/*");
        let mut a = Agent::new(
            AgentId::new("a1"),
            AttentionBudget::new(100, 1000),
            Arc::new(InMemorySpace::new()),
            Arc::new(MockProvider::new("ok", 5)),
        )
        .with_verifier(signer.clone())
        .grant(forged);
        a.start().unwrap();
        let err = a
            .act(Action::Remember {
                object: obj("n1", vec![1.0, 0.0]),
                cost: 3,
            })
            .await
            .unwrap_err();
        // Rejected despite matching permission + scope — the signature fails.
        assert!(matches!(err, TamError::CapabilityDenied(_)));

        // A properly issued token with the same grant is accepted.
        let good = signer.issue(cap("a1", Permission::Write, "mem://a1/*"));
        let mut a2 = Agent::new(
            AgentId::new("a1"),
            AttentionBudget::new(100, 1000),
            Arc::new(InMemorySpace::new()),
            Arc::new(MockProvider::new("ok", 5)),
        )
        .with_verifier(signer)
        .grant(good);
        a2.start().unwrap();
        assert!(
            a2.act(Action::Remember {
                object: obj("n1", vec![1.0, 0.0]),
                cost: 3,
            })
            .await
            .is_ok()
        );
    }

    #[tokio::test]
    async fn run_loops_tool_call_then_answers() {
        use thaliox_cognition::{Completion, ToolCall};
        // Scripted model: first ask to call `echo`, then answer in text.
        let mind = MockProvider::scripted(vec![
            Completion::calls(
                20,
                vec![ToolCall {
                    id: "c1".into(),
                    name: "echo".into(),
                    arguments: r#"{"input":"hi"}"#.into(),
                }],
            ),
            Completion::text("final answer", 10),
        ]);
        let mut a = Agent::new(
            AgentId::new("a1"),
            AttentionBudget::new(10_000, 100_000),
            Arc::new(InMemorySpace::new()),
            Arc::new(mind),
        )
        .with_tool(Arc::new(EchoTool))
        .grant(tool_cap());
        a.start().unwrap();

        let answer = a.run("use the echo tool", 5).await.unwrap();
        assert_eq!(answer, "final answer");
        // Audit: think → tool invoke → think.
        let ops: Vec<Operation> = a.audit().iter().map(|r| r.op).collect();
        assert_eq!(
            ops,
            vec![Operation::Think, Operation::ToolInvoke, Operation::Think]
        );
    }
}
