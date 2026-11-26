/**
 * Rex Program Status Extension for pi
 *
 * Reports pi's state to the Rex terminal with OSC 7501 program-status
 * sequences, the same way Claude Code does: `working` while the agent
 * runs, `idle` once it settles at the prompt, and `blocked` while an
 * extension UI prompt (confirm / select / input) waits for the user.
 *
 * smarthistory's `*` view reads these records and colors a pi pane
 * that is waiting for input (see docs/modes/panes.md).
 *
 * Install: copy to ~/.pi/agent/extensions/rex-status.ts
 * Does nothing outside Rex (no REX_BLOCK in the environment), so other
 * terminals never see an unknown escape sequence.
 */

import type { ExtensionAPI } from "@mariozechner/pi-coding-agent";

type State = "working" | "idle" | "blocked" | "clear";

function report(state: State, kind?: string): void {
	if (!process.env.REX_BLOCK || !process.stdout.isTTY) return;
	const fields = [`state=${state}`];
	if (state !== "clear") fields.push("app=pi");
	if (state === "blocked" && kind) fields.push(`kind=${kind}`);
	process.stdout.write(`\x1b]7501;${fields.join(":")}\x1b\\`);
}

export default function (pi: ExtensionAPI) {
	pi.on("session_start", async () => report("idle"));
	pi.on("agent_start", async () => report("working"));
	// `agent_end` can still be followed by retries or queued messages;
	// `agent_settled` means pi is really back at the prompt.
	pi.on("agent_settled", async () => report("idle"));
	pi.on("ui_prompt_start", async (event) =>
		report("blocked", event.kind === "confirm" ? "permission" : "question"),
	);
	pi.on("ui_prompt_end", async (_event, ctx) => report(ctx.isIdle() ? "idle" : "working"));
	pi.on("session_shutdown", async () => report("clear"));
}
