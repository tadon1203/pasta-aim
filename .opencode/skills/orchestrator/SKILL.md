---
name: orchestrator
description: Use when an effort spans several work units, or when the operator asks for orchestration, decomposition, or parallel progress. The model makes the decisions and subagents do the work.
metadata:
  opencode/autoinvoke: false
---

# Orchestrate the effort

You make the decisions. Subagents do the work.

Divide the task into work units. For each unit, give the subagent the question that it must
answer and the rules that it must keep. When the subagent returns, judge its evidence. Keep
conclusions in your context, not large data.

This split is a default, not a rule. Do a small piece of work yourself when the delegation costs
more time than the work.
