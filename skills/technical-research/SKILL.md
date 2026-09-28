---
name: technical-research
description: Use when an engineering decision needs evidence from specifications, source code or a bounded experiment.
---

# Technical research

Frame a concrete question and the decision it will inform. Record constraints,
candidate solutions and what evidence would change the choice. Set a stopping
condition so research does not expand indefinitely.

Start with available project evidence, then primary sources: specifications,
RFCs, official documentation, release notes, source and relevant research papers.
Match library/runtime versions to the target environment and check dates for
claims that can change. Distinguish normative requirements from implementation
behavior and informal recommendations.

Keep an evidence ledger: claim, supporting source/version, observation and
uncertainty. Read the relevant section before citing it; a search snippet is not
sufficient support. Resolve conflicting sources by scope and version rather than
silently selecting the convenient one.

Use a small proof of concept when documentation cannot answer an important
question. Specify the hypothesis, inputs, expected discriminator and resource
limit. Isolate artifacts and secrets. A prototype proves only what it exercises;
do not present it as production-ready implementation.

Compare alternatives against hard requirements first, then the tradeoffs that
matter. Separate measured results, source-backed facts, estimates and inference.

Return a recommendation with direct source references, experimental evidence,
limitations and remaining questions. Stop when the evidence supports the required
decision or name the external information that prevents it.
