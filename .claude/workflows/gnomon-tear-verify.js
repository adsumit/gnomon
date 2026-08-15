export const meta = {
  name: 'gnomon-tear-verify',
  description: 'Adversarially verify the M5 tear-off diagnosis and fix in ~/dev/gnomon',
  phases: [
    { title: 'Investigate' },
    { title: 'Verify' },
    { title: 'Synthesize' },
  ],
}

const REPO = '/home/adsumit/dev/gnomon'

const CONTEXT = `
Repo: ${REPO} (Rust, GTK4 + wlr-layer-shell). NEVER touch ~/dev/plinth. Read-only task: do NOT edit any file.

The working tree contains an UNCOMMITTED fix. HEAD (84e156a) is the buggy version.
- Current (fixed) code:  read ${REPO}/crates/gnomon-gtk/src/app.rs, window.rs, geom.rs
- Previous (buggy) code: run \`git -C ${REPO} show HEAD:crates/gnomon-gtk/src/app.rs\` (and window.rs / geom.rs)
- The change itself:     run \`git -C ${REPO} diff\`

Background. A "Panel" is one wlr-layer-shell surface anchored Top+Left, so its \`margins\` ARE its absolute
monitor-space position. \`Layout\` owns Vec<Rc<Panel>>. Dragging a row out of a multi-row panel "tears" it
into a new panel; the original is the SOURCE panel. On drag release, a panel within 60px of another merges
into it. A single GestureDrag lives on the source panel's overlay; after a tear the source sets
\`drag_target\` and forwards subsequent drag-update offsets to the new panel.

Two defects were reported by a real run of HEAD:
  A) The torn-off panel shows "Loading usage" indefinitely.
  B) The SOURCE panel does not stay where it was - it moves or resizes when the row is removed.
`

const FINDING = {
  type: 'object',
  additionalProperties: false,
  required: ['summary', 'evidence', 'confidence'],
  properties: {
    summary: { type: 'string', description: 'One sentence: the claim.' },
    evidence: { type: 'string', description: 'Exact file:line references and quoted code that support it.' },
    confidence: { type: 'string', enum: ['high', 'medium', 'low'] },
  },
}

const REPORT = {
  type: 'object',
  additionalProperties: false,
  required: ['headline', 'findings'],
  properties: {
    headline: { type: 'string' },
    findings: { type: 'array', items: FINDING },
  },
}

const LENSES = [
  {
    key: 'cold-derivation',
    prompt: `${CONTEXT}

Your job: WITHOUT being told the answer, derive from HEAD's code (the buggy version) exactly what writes the
SOURCE panel's margins or size during a tear gesture.

Method: enumerate EVERY code path that can write \`Panel::margins\` or call \`set_default_size\` /
\`set_margin\`. For each, decide whether it can run during a tear of a two-row panel, and in what order.
Pay close attention to the ORDER of events inside \`connect_drag_update\` in HEAD's wire_drag.
Report the specific write(s) that displace the source panel, with file:line from HEAD's version.`,
  },
  {
    key: 'refute-claim',
    prompt: `${CONTEXT}

A claim has been made. Your job is to REFUTE it. Default to "refuted" if the evidence is not airtight.

CLAIM: In HEAD, \`connect_drag_update\` calls \`panel.apply_move(dx, dy)\` on the SOURCE panel for every
drag-update event whose \`dx.hypot(dy)\` has not yet exceeded TEAR_THRESHOLD (40). Those calls write new
margins onto the source. When the threshold is finally crossed, the tear happens and the source is
abandoned at its displaced position - up to ~40px from where it started - and nothing ever restores it.
Therefore \`apply_move\` in the pre-tear phase is the write that moved the source panel.

Attack it. Is \`apply_move\` actually reached in that window? Does anything later undo it? Could
drag-update fire only once past the threshold? Does \`drag_row\` really get set for a two-row panel?
Trace \`wire_drag\`'s drag_begin -> drag_update ordering in HEAD precisely and state whether the claim
survives, and if not, why.`,
  },
  {
    key: 'candidate-elimination',
    prompt: `${CONTEXT}

Rule each of these five hypotheses for defect B strictly IN or OUT by reading HEAD's code. For each, give
file:line and a one-line verdict. Do not hedge; if a hypothesis cannot fire during a tear, say so and prove it.

1. \`auto_place\` running again on a later map or realize.
2. A stale \`target\` left set from the drag, applied by \`tick_resize\` after the tear.
3. The probe's \`on_allocation\` updating \`last_allocated\` and re-triggering a resize.
4. Snap-on-release being applied to the SOURCE panel rather than the torn one.
5. \`set_default_size\` being re-issued with the pre-tear size.

Also answer separately: when the source panel loses a row, does its layer surface legitimately shrink
(fewer rows -> smaller natural size), and can that shrink also move it given Top+Left anchoring?`,
  },
  {
    key: 'snapshot-cache-audit',
    prompt: `${CONTEXT}

Audit ONLY the fix for defect A in the CURRENT working tree: the \`Layout::last_snapshot\` cache.

Check rigorously:
- Is the cache populated before any panel could be constructed from it? (see \`Layout::dispatch\`)
- Is it applied in \`Panel::new\` genuinely BEFORE the window is first shown (\`win.present()\`)?
- RefCell hazards: can \`last_snapshot.borrow()\` in \`Panel::new\` or \`Layout::merge\` ever be live at the
  same time as \`last_snapshot.borrow_mut()\` in \`dispatch\`, causing a runtime panic? Trace whether
  \`Panel::new\` can be reached re-entrantly from inside \`dispatch\`.
- Same question for \`Layout::panels\` borrows across \`merge\` -> \`remove\` -> \`retain\`.
- Does \`Content::apply_snapshot\` in window.rs actually clear the "Loading usage" placeholder when seeded?
  Follow \`loaded\`, \`render()\`, and the \`nothing_for_us || unchanged\` early-out precisely. Is there any
  input for which seeding leaves the panel showing "Loading usage"? Consider a torn panel whose single kind
  is present in the snapshot, and one whose kind is ABSENT from the snapshot.
- Is the re-apply inside \`Layout::merge\` correct, or could it undo the \`absorb\` that precedes it?`,
  },
  {
    key: 'regression-audit',
    prompt: `${CONTEXT}

Audit the CURRENT working tree's restructured \`wire_drag\` for regressions against HEAD. Run
\`git -C ${REPO} diff\` first.

For each of these behaviours, state whether it is preserved, changed, or broken, with file:line:
- Plain move of a SINGLE-row panel (drag_row is never set).
- Plain move of a MULTI-row panel started on the status label / empty area (kind_at returns None).
- Edge and corner resize (zone.is_resize()).
- Right-button resize.
- Snap on release.
- Merge on release.
- Pin (middle click) and the pinned early-returns.
- The post-tear redirect: does the source still stay put for the REST of the gesture?

Then look specifically for NEW problems the restructure could introduce:
- The new \`uncommitted\` early-return in drag_end: does it skip anything that MUST still happen (e.g. is
  \`set_resizing(false)\` still called on every path that called \`set_resizing(true)\`)?
- A drag on a tear-candidate row that stays under 40px: what does the user see? Is the panel now
  completely immovable when grabbed on a row?
- Can \`drag_row\` be left stale across gestures?
- Any RefCell double-borrow: \`drag_row\`, \`drag_target\`, and window.rs's \`state\` are all RefCells, and
  \`tear_off\` calls \`content.remove_kind\` which re-enters window.rs. Prove no borrow is held across it.`,
  },
]

phase('Investigate')
const reports = (await parallel(
  LENSES.map((l) => () =>
    agent(l.prompt, { label: l.key, phase: 'Investigate', schema: REPORT })
  )
)).filter(Boolean)

const claims = []
reports.forEach((r, i) => {
  const lens = LENSES[i] ? LENSES[i].key : `lens-${i}`
  for (const f of r.findings || []) claims.push({ lens, ...f })
})

log(`${reports.length} investigations returned ${claims.length} claims`)

const VERDICT = {
  type: 'object',
  additionalProperties: false,
  required: ['stands', 'reason'],
  properties: {
    stands: { type: 'boolean', description: 'true if the claim survives scrutiny against the actual code' },
    reason: { type: 'string' },
    correction: { type: 'string', description: 'If it does not stand, what is true instead. Empty otherwise.' },
  },
}

phase('Verify')
const verified = await parallel(
  claims.slice(0, 24).map((c, i) => () =>
    agent(
      `${CONTEXT}

Verify this claim against the actual code. Be skeptical: read the file yourself and confirm every line
reference. If the claim is directionally right but the file:line or mechanism is wrong, it does NOT stand.

CLAIM (from the "${c.lens}" investigation): ${c.summary}
STATED EVIDENCE: ${c.evidence}`,
      { label: `verify-${i}`, phase: 'Verify', schema: VERDICT }
    ).then((v) => ({ ...c, verdict: v }))
  )
)

const survivors = verified.filter(Boolean).filter((c) => c.verdict && c.verdict.stands)
const refuted = verified.filter(Boolean).filter((c) => c.verdict && !c.verdict.stands)
log(`${survivors.length} claims stand, ${refuted.length} refuted`)

phase('Synthesize')
const summary = await agent(
  `${CONTEXT}

You are writing the final verification verdict. Below are claims that survived independent scrutiny, and
claims that were refuted. Read the code yourself to resolve any contradiction between them.

SURVIVED:
${JSON.stringify(survivors.map((c) => ({ lens: c.lens, summary: c.summary, reason: c.verdict.reason })), null, 1)}

REFUTED:
${JSON.stringify(refuted.map((c) => ({ lens: c.lens, summary: c.summary, correction: c.verdict.correction })), null, 1)}

Produce:
1. The single root cause of defect B, stated as "X wrote the source panel's margins at Y".
2. Whether the working tree's fix actually prevents it. Yes or no, with the reason.
3. Any REMAINING defect or regression in the working tree that a maintainer must fix before commit,
   ranked most severe first. Only include things you have verified in the code yourself. If there are
   none, say so plainly.
4. Anything about defect A's fix that is still wrong.`,
  { label: 'synthesis', phase: 'Synthesize' }
)

return { survivors: survivors.length, refuted: refuted.length, summary }
