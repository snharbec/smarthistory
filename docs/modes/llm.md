# LLM command mode (`=`)

| Default prefix | `=` |
| --- | --- |
| Configurable | `prefix.llm=<char>` |

LLM command mode sends the body of the query (everything after `=`) to the configured ollama instance as a natural-language description, asks the model for a Bash one-liner, and stages the result for execution. Useful when you know *what* you want to do but not the exact command.

## What it does

- `=find duplicate files in the current directory`
- After 1 second of typing inactivity the request fires.
- The model's reply is shown as a preview row at the *top* of the result list, marked with a `[LLM]` chip in accent color and a `~` exit marker (it's a suggestion, not a real command).
- The preview is updated as you keep typing (debounced).

## Selecting a row

- `Enter` on the preview row stages the model's command as the next selection and exits the TUI. The parent shell runs it.
- `Enter` on a *real* history row (the ones below the preview) stages that history row's command instead.
- `Ctrl-K` (Describe) sends the selected history row's `command` to the LLM with a "what does this do?" prompt and shows the 4-sentence answer in an overlay.
- `Ctrl-T` (Correct) is the inverse: it sends the selected command + the model's correction to the LLM and stages the corrected command.

## Debounce

The request is debounced: 1 second after the last keystroke. The debounce is armed by every text-mutating action (typing, backspace, paste). A long pause doesn't help — the request fires the moment you've paused for 1s.

## Cursor position

Unlike other modes, the cursor in LLM mode is freely positionable. `Left` / `Right` move the cursor character by character; insert lands at the cursor (not just the end). The escape key (`Ctrl-C` / `Esc`) cancels the request without leaving the TUI.

## Configuration

- `ollama.url=http://127.0.0.1:11434`
- `ollama.model=qwen2.5-coder:7b`

Both must be set, otherwise `=` mode is a no-op and the status bar shows `LLM not configured`. See [Configuration](../../README.md#configuration) for the full list of config keys.

## Cross-references

- [Question mode — a sibling LLM mode that shows the answer in an overlay rather than staging a command](question.md)
- [TECHNICAL — LLM command-generation details](../../TECHNICAL.md#llm-command-generation)
