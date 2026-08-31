---
name: decision-matrix
description: Work a fork in the road out loud — the options, what separates them, a weighted score, and whether the winner survives moving the weights.
version: 1.0.0
license: MIT
keywords: [decision, tradeoff, options, criteria, weighting]
requires:
  agent: ">=0.1.0"
---

# Deciding between options

Use this when the work reaches a fork that is not obvious: a library, a storage
shape, a protocol, a migration order. Not for a choice with one sensible answer —
a matrix around a foregone conclusion is theatre, and it teaches whoever reads it
to skim the next one.

The point is not the number at the bottom. It is that the criteria and the
weights are written down *before* the scores, so a preference has to argue for
itself instead of arriving as a total.

## 1. Say what is being decided, and list the options

One sentence: "choose X for Y, given Z". Then three to seven options. Fewer than
three is usually a decision already made; more than seven is a matrix nobody
reads. Include the option of doing nothing when it is real.

## 2. Drop what fails a hard requirement

List the must-haves first — licence, platform, a budget, an interface that has to
keep working. An option that fails one is out before it is scored. Otherwise
strong marks elsewhere carry something that was never eligible.

Say which option went and on which requirement. That is the most re-litigated
part of any decision.

## 3. Choose five to eight criteria

Independent ones. Two criteria that measure the same thing — "speed" and
"performance" — count it twice and double its weight without saying so.

## 4. Weight them, to a total of 1.0

If the weights are hard to set, compare in pairs: for each pair of criteria ask
which matters more here, count the wins, normalise. Ties are allowed.

## 5. Fix the scale and say what its middle means

1–5 or 1–10, with a line for what a 3 is. Without that, scores drift between
rows: the same 3 means "adequate" in one and "the best of a poor field" in the
next.

**Every scale runs the same way: higher is better.** Cost and risk are the ones
that catch people — score "cost we can live with", not "cost".

## 6. Score, and total

`total = Σ (weight × score)`. Show the table.

| Criterion | Weight | A | B | C |
|---|---|---|---|---|
| Cost of ownership | 0.30 | 4 | 2 | 5 |
| Time to working | 0.25 | 3 | 5 | 2 |
| Support and community | 0.25 | 5 | 4 | 2 |
| Room to change later | 0.20 | 2 | 4 | 3 |
| **Total** | **1.00** | **3.60** | **3.55** | **3.15** |

## 7. Move the weights and see whether the winner moves

Shift each weight by a fifth and re-total. If the leader changes, the decision is
resting on the weights rather than on the evidence — say so, and go and find the
missing fact instead of reporting a winner.

Two options within about 0.1 of each other are a tie. The honest conclusion is
"A and B are equivalent; decide on something the matrix does not hold", not "A
wins by 0.05". Three decimal places do not make an estimate objective.

## 8. Put it to whoever is deciding

Recommend one, in a sentence, with the reason that actually decided it — usually
one or two criteria, not the total.

Then use `ask` with the options as choices. Whoever answers can pick one or type
something else entirely, and an answer that is not on the list is the most
valuable thing this produces: it means the fork was framed wrongly, and that is
worth more than a score.

Do not run a matrix, announce the winner and proceed. The matrix exists to be
disagreed with.

## What goes wrong

- **Scoring towards an answer already chosen.** The defence is order: options,
  requirements, criteria and weights are fixed and stated before a single score
  is written.
- **A criterion nobody can score.** If two people would give it wildly different
  numbers, it is not a criterion yet — split it or drop it.
- **Precision that is not there.** The matrix structures the argument and shows
  where the disagreement is. It does not produce a right answer.
