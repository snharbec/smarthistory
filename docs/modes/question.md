# Question mode (`%`)

| Default prefix | `%` |
| --- | --- |
| Configurable | `prefix.question=<char>` |

Question mode sends the body of the query to the configured ollama instance and shows the model's 4-sentence answer in an overlay. Useful for short factual questions where you don't want a Bash command — you want a text reply you can read.

## What it does

- `%when was TCP invented`
- After 1 second of typing inactivity the request fires.
- The model's reply is shown in a scrollable overlay (a `QuestionView`), not staged for execution. Press `Esc` (or the configured `Cancel` key) to close the overlay.

## Selecting a row

In question mode the result list is mostly empty — question mode is an overlay, not a list. The `Enter` action stages the question for re-execution rather than selecting a row.

`Ctrl-K` (Describe) is the sibling action for history rows: it shows the LLM's 4-sentence summary of what the selected command does.

## Debounce

Same as LLM command mode: 1 second after the last keystroke. `Ctrl-C` / `Esc` cancels the request without leaving the TUI.

## Configuration

Same as LLM command mode: requires `ollama.url` + `ollama.model`. Without both, `%` mode is a no-op.

## Cross-references

- [LLM command mode — the sibling LLM mode that stages a Bash command](llm.md)
