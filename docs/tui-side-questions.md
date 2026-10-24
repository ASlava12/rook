# Side questions while a turn is running

In an open TUI conversation, `/btw <question>` asks about the current work while
the foreground turn continues. It uses one auxiliary model request without tools.
The question never enters the foreground correction queue and its completion
does not finish or replace the running turn. The selected model is captured when
the question is submitted; pending foreground model changes are not consumed.
Provider concurrency limits still apply. A named endpoint defaults to one
in-flight model request; a side question then waits for that request to finish.
It can answer during a tool or user-input wait, or concurrently with model work
when the endpoint's configured `parallel` limit permits another request.

In a new local window, a first valid `/btw` creates an empty conversation for
its auxiliary transcript note, preserving the existing local startup behavior.
It does not start a foreground turn. Invalid questions keep their draft and do
not create a conversation. A window still awaiting its foreground session ID
keeps the side-question draft until that session is available.

One side question can run per window. Sending another keeps its draft and shows
that the first is still running. Answers and failures appear with their original
question, without replacing text currently being edited. The exchange is saved
as an auxiliary transcript note and excluded from normal conversation replay.
The bounded answer is displayed after completion so known credentials can be
masked even when they span several model fragments. Request messages, saved
question/answer text and provider errors use the existing known-value mask.

Changing sessions or closing the window cancels the window's auxiliary request.
A routed window uses the daemon's side-question API; dropping its response body
cancels that auxiliary future. A daemon without the endpoint reports an error.
The client does not resubmit the question as a normal foreground prompt.

Questions are limited to 8192 UTF-8 bytes, with a three-minute request deadline
and at most four concurrent auxiliary questions per process. The remote client
accepts at most 256 KiB of response data. API request bodies have a 64 KiB limit.

This implementation is independent of Hermes. Its early `/btw` dispatch before
busy-turn queue handling informed the routing review.
