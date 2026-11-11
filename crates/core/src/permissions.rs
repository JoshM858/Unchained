//! The permission engine. Every tool call is reduced to a (tool, effect)
//! pair and resolved to allow, ask or deny. Explicit rules win, in order;
//! otherwise the session mode decides.

use std::str::FromStr;

/// What a tool does to the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    Read,
    Write,
    Execute,
    Network,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Ask,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Read-only: anything that changes the host is denied.
    Plan,
    /// Reads are free; everything else asks.
    #[default]
    Ask,
    /// File edits are free; commands and network still ask.
    AutoEdit,
    /// Nothing asks. Only sensible once the sandbox exists.
    FullAuto,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Plan => "plan",
            Mode::Ask => "ask",
            Mode::AutoEdit => "auto-edit",
            Mode::FullAuto => "full-auto",
        }
    }
}

impl FromStr for Mode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "plan" => Ok(Mode::Plan),
            "ask" => Ok(Mode::Ask),
            "auto-edit" => Ok(Mode::AutoEdit),
            "full-auto" => Ok(Mode::FullAuto),
            other => Err(format!(
                "unknown mode '{other}' (expected plan, ask, auto-edit or full-auto)"
            )),
        }
    }
}

/// A rule matches when every field that is set matches the request.
#[derive(Debug, Clone)]
pub struct Rule {
    pub tool: Option<String>,
    pub effect: Option<Effect>,
    pub verdict: Verdict,
}

#[derive(Debug, Clone, Default)]
pub struct Policy {
    pub mode: Mode,
    pub rules: Vec<Rule>,
}

impl Policy {
    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
            rules: Vec::new(),
        }
    }

    pub fn decide(&self, tool: &str, effect: Effect) -> Verdict {
        for rule in &self.rules {
            let tool_ok = rule.tool.as_deref().is_none_or(|t| t == tool);
            let effect_ok = rule.effect.is_none_or(|e| e == effect);
            if tool_ok && effect_ok {
                return rule.verdict;
            }
        }
        match (self.mode, effect) {
            (_, Effect::Read) => Verdict::Allow,
            (Mode::Plan, _) => Verdict::Deny,
            (Mode::Ask, _) => Verdict::Ask,
            (Mode::AutoEdit, Effect::Write) => Verdict::Allow,
            (Mode::AutoEdit, _) => Verdict::Ask,
            (Mode::FullAuto, _) => Verdict::Allow,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_defaults() {
        use Effect::*;
        use Verdict::*;
        let cases = [
            (Mode::Plan, [Allow, Deny, Deny, Deny]),
            (Mode::Ask, [Allow, Ask, Ask, Ask]),
            (Mode::AutoEdit, [Allow, Allow, Ask, Ask]),
            (Mode::FullAuto, [Allow, Allow, Allow, Allow]),
        ];
        for (mode, expected) in cases {
            let p = Policy::new(mode);
            for (effect, want) in [Read, Write, Execute, Network].into_iter().zip(expected) {
                assert_eq!(p.decide("t", effect), want, "{mode:?} {effect:?}");
            }
        }
    }

    #[test]
    fn first_matching_rule_wins() {
        let mut p = Policy::new(Mode::FullAuto);
        p.rules.push(Rule {
            tool: Some("write_file".into()),
            effect: None,
            verdict: Verdict::Deny,
        });
        p.rules.push(Rule {
            tool: None,
            effect: Some(Effect::Write),
            verdict: Verdict::Ask,
        });
        assert_eq!(p.decide("write_file", Effect::Write), Verdict::Deny);
        assert_eq!(p.decide("patch", Effect::Write), Verdict::Ask);
        assert_eq!(p.decide("shell", Effect::Execute), Verdict::Allow);
    }
}
