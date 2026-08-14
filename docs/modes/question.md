# Question mode (`?`)

| Default prefix | `?`                      |
| -------------- | ------------------------ |
| Configurable   | `prefix.question=<char>` |

Question mode sends the body of the query — plus context about the last command
you ran, if any — to the configured ollama instance and shows the model's
4-sentence answer. Useful for short factual questions where you don't want a
Bash command — you want a text reply you can read. Reachable two ways: from
[the raw shell prompt](#from-the-shell-prompt-no-tui) (no TUI needed) or from
inside the TUI, described below.

## What it does (inside the TUI)

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

## From the shell prompt (no TUI)

Type `?question text` directly at the normal zsh prompt — not inside the TUI's
own input box — and press `Enter`. `smarthistory` intercepts the line at
`accept-line` (before it would ever be handed to the real shell for execution —
a `?...` line isn't a valid command and never runs one), calls the LLM
synchronously, and prints the answer straight to the console:

```
$ git status
# (git reports a conflict)
$ ?why did that fail

LLM Answer
The command failed because ...
```

A blank line, then a transient `Thinking…` line, lets you know the request is in
flight (a local model typically takes 1-5 seconds). On an interactive terminal
`Thinking…` is then **replaced in place** by the `LLM Answer` header and the
answer text — it doesn't linger above the answer in your scrollback (the static
example above can't show that "replace" motion, only the end result). On a
non-TTY (piped/redirected output), there's no cursor to move, so `Thinking…`
simply stays as its own line above the answer instead. Colorized (magenta
header, dim `Thinking…`) when the terminal supports it. Same automatic
last-command context as the TUI path above — `?why did that fail` right after a
failing command works without repeating the command.

If the answer includes one or more suggested commands, they're offered as a
numbered pick list:

```
$ ?how do I undo that

LLM Answer
You can undo the last commit without losing your changes.
1) git reset --soft HEAD~1
Choose [1-1], Enter to skip: 1
$ git reset --soft HEAD~1
```

Typing a number and pressing `Enter` stages that command into the next prompt
for you to review and run yourself — it is **never** run automatically, same
convention as every other LLM-generated command in this project (`=` mode,
`Ctrl-T` correct). Pressing `Enter` with no number skips the pick list and
leaves the prompt empty.

The question is recorded to the same `history` table as a TUI-asked question
(`mode = 'question'`), so it shows up in `?`-mode search and
`smarthistory project report` identically either way. The line is also added to
zsh's own interactive history (`Ctrl-P`/Up-arrow recalls it, without having
"run" it).

Requires the same `ollama.url` + `ollama.model` configuration as the TUI path;
without it, `?question<Enter>` prints a "not configured" message instead of the
historical "command not found" shell error.

## Configuration

Same as LLM command mode: requires `ollama.url` + `ollama.model`. Without both,
`?` mode is a no-op.

## Cross-references

- [LLM command mode — the sibling LLM mode that stages a Bash command](llm.md)
