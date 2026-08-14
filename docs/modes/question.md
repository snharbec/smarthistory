# Question mode (`?`)

| Default prefix | `?`                      |
| -------------- | ------------------------ |
| Configurable   | `prefix.question=<char>` |

Question mode sends the body of the query — plus context about the last command
you ran, if any — to the configured ollama instance and shows the model's
4-sentence answer in an overlay. Useful for short factual questions where you
don't want a Bash command — you want a text reply you can read.

## What it does

- `?when was TCP invented` — a standalone factual question, no command context
  needed.
- `?what does that do` / `?why did that fail` / `?how would you do this instead`
  — the last command run in the current session (its command line, exit code,
  and captured output, if any) is automatically included as context, so these
  resolve "that"/"this"/"it" without retyping the command.
- Press `Enter` (`Run`) or `Tab` (`JiraFieldComplete` — a no-op elsewhere in
  this mode since there's nothing to complete) to send the request. Unlike LLM
  command mode, question mode does **not** auto-fire on typing inactivity — you
  decide when the question is ready to send.
- The model's reply is shown in a scrollable overlay (a `QuestionView`), not
  staged for execution. Press `Esc` (or the configured `Cancel` key) to close
  the overlay.

## Selecting a row

In question mode the result list is mostly empty — question mode is an overlay,
not a list. Both `Enter` and `Tab` fire the LLM request rather than selecting a
row.

`Ctrl-K` (Describe) is the sibling action for history rows: it shows the LLM's
4-sentence summary of what the selected command does.

## Cancelling

`Ctrl-C` / `Esc` cancels an in-flight request without leaving the TUI.

## Configuration

Same as LLM command mode: requires `ollama.url` + `ollama.model`. Without both,
`?` mode is a no-op.

## Cross-references

- [LLM command mode — the sibling LLM mode that stages a Bash command](llm.md)
