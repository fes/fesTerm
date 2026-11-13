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
- The map holds three kinds of generated page: one routing page per domain,
  a glossary of project terms, and state models (`state-*.md`, declared under
  `state_models` in [domains.json](domains.json)).
- The glossary is refreshed when its cited source files or the manifest
  settings change. A state model's states and terminal set are parsed from the
  enum and its `is_terminal`; the model proposes transitions, and a transition
  is kept only if its cited symbol is a non-test function in a production
  file whose own body names the target state (comments and `#[cfg(test)]`
  items are ignored). Graph property checks (reachability, no exits from
  terminal states, no dead ends, a path to a terminal state) are computed by
  the script, not the model. A FAIL is a lead for review, not proof of a bug,
  and no state model is checked against runtime traces. A state page is stale
  when the enum, its cited files, or any production line naming a variant
  changes.
- The glossary hashes only the files it cites, so vocabulary added elsewhere
  does not trigger regeneration; a state page misses transition logic that
  changes without touching a line naming a variant. A monthly scheduled run (or a
  dispatch with `refresh_coverage`) regenerates them with
  `regenerate --refresh-coverage` and opens a PR from an
  `agent-navigation/refresh-*` branch. This requires the repository setting
  that allows GitHub Actions to create pull requests. Close the PR if it only
  rewords entries.

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
and cost independently.

## Pilot result (2026-11-12)

[`scripts/agent_navigation_eval.py`](../../scripts/agent_navigation_eval.py)
ran a paired file-localization pilot at snapshot `d383f618`: 40 merged PRs
from September to November 2026 across all 12 domains, each changing at most
four non-test Rust source files. A separate model wrote each question from the
PR description without paths or identifiers, and the gold answer was the
files the PR changed. Arm A had `docs/agent-navigation` removed; arm B had the
map plus the pointer now in `AGENTS.md`. Both used `gemini-4-argon` with view,
grep, and glob only, three fresh repeats per task, in random order (240 runs,
0 errors). Scoring was deterministic and blind to the arm. Tasks, gold
answers, and raw results were kept outside the repository.

| Metric | No map | Map |
| --- | --- | --- |
| Top-1 file is a gold file | 92.5% | 93.3% |
| Gold-file recall | 92.7% | 91.4% |
| Precision | 55.5% | 58.4% |
| Median input tokens | 56,546 | 45,100 |
| Median output tokens | 1,673 | 1,232 |
| Median seconds | 119 | 108 |
| Premium requests per run | 1 | 1 |

Per task, the map arm used fewer input plus output tokens on 28 of 40 tasks
(sign test p = 0.017). The geometric-mean ratio was 0.80 (bootstrap 95% CI
0.73 to 0.88), and total tokens including cache reads were 27% lower. Recall
tied on 36 tasks and was slightly lower with the map on 4 (mean -1.3 points).

Pre-set gates: routing +10 points failed, because the baseline was already at
92.5%; a quality drop of at most 5 points passed; cost per success not rising
passed. Verdict: **keep and refine**. On localization questions of this
difficulty, the map saves tokens and time without measurably improving
answers. Billed premium requests are unchanged under per-request pricing, and
maintenance costs about one request per stale page per PR. Next, test harder,
cross-boundary tasks (mobile and terminal-core were the weakest), measure
amortized maintenance cost, and remove the map if token savings fall below
its maintenance cost.
