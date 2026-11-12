# Agent Navigation: Evidence and Evaluation

This is background for evaluating the [retrieval map](README.md),
not context needed for ordinary code navigation.

## Research

- [Evaluating AGENTS.md](https://arxiv.org/html/2602.11988v3#S4.SS3) tested
  both AI-generated and developer-written general context files on Python
  issue-resolution tasks. It associated higher cost with additional testing,
  exploration, tool use, and reasoning. It did not isolate a source-grounded,
  PR-maintained retrieval map or establish its effect on fesTerm.
- [Vercel's evaluation](https://vercel.com/blog/agents-md-outperforms-skills-in-our-agent-evals)
  found a compressed index of Next.js documentation in `AGENTS.md` effective
  for its specific Next.js 16 tasks; it is not a general cost guarantee.
- [RepoGraph](https://arxiv.org/html/2410.14684v2) evaluated a dynamic
  code-dependency graph, not this Markdown map. It improved issue resolution
  in its reported comparisons, while average tokens also increased.
  [SWE-Explore](https://arxiv.org/abs/2606.07297) evaluates relevant-line
  coverage, ranking, and context efficiency directly.
- [Aider's repository map](https://aider.chat/docs/repomap.html) describes
  task-relevant symbols and a token budget.
  [Anthropic's context guidance](https://www.anthropic.com/research/effective-context-engineering-for-ai-agents)
  recommends high-signal context and just-in-time retrieval.

These are motivations for testing the map, not evidence that it reduces
fesTerm's token use or improves its answers.

## Operation

- The `Agent navigation` workflow runs on every non-draft same-repository
  PR. A page is regenerated when its domain files, canonical docs, manifest
  entry, or prompt/generator version change, or when a link or symbol it cites
  breaks. Otherwise the run makes no model call.
- Generation runs in a job with only `contents: read` and
  `copilot-requests: write`; a separate publish job commits the validated
  pages to the PR branch and dispatches CI, because pushes made with
  `GITHUB_TOKEN` do not trigger workflows. Pull before pushing again.
- Personal repositories need an `AGENT_NAVIGATION_COPILOT_TOKEN` secret: a
  fine-grained PAT whose only permission is "Copilot Requests".
- Draft and fork PRs are not regenerated. Fork PRs get a warning-only check,
  and `main` fails its check if the map is stale.
- If two PRs conflict in a generated page, resolve by taking either side; the
  page's source hash then mismatches and the next run regenerates it.
- Generated text is untrusted. It is reviewed in the PR diff like code, and
  the generator drops text that addresses agents.
- The glossary page is generated separately and refreshed when its cited source
  files or the manifest settings change; if it is stale, the same freshness
  check fails until the page is regenerated.

## Opt-in comparison

1. Prepare representative, independently authored questions and gold source
   regions for multiple domains and cross-boundary flows. Keep exact prompts
   and gold answers out of the map and blind graders to the condition.
2. Compare the same underlying code snapshot, model, tools, settings, and
   prompts: baseline instructions without the map or its pointers versus the
   same instructions with them. Preserve all existing policy in both. Use
   fresh contexts, repeat trials, and vary their order.
3. Record answer correctness, wrong-boundary and missing-evidence failures,
   target-symbol recall, relevant-line coverage, and ranking. Count failed
   tasks; cheaper but incomplete answers do not count as wins.
4. Record input and output tokens, cache hits/writes when available, inference
   cost, retrieval/tool-call counts, retries, and elapsed time. Report cold and
   cached results separately. Include initial map-generation and incremental
   PR-maintenance costs when assessing amortized savings.

One simple answer rubric is 0 for an incorrect anchor or boundary, 1 for a
partly correct answer missing important evidence or constraints, and 2 for
correct anchors, neighboring evidence, and relevant caveats. Report quality
and cost independently; no fesTerm A/B result is claimed here.
