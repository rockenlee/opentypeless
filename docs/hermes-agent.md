# Hermes Agent Integration

OpenTypeless can route explicit voice commands to a local Hermes Agent instead of treating them as normal dictation.

## Trigger phrases

The transcript must start with one of these prefixes:

- `hermes `
- `agent `
- `ask hermes `
- `ask agent `

Examples:

```text
hermes summarize the selected text
agent run a quick diagnosis of this project
ask hermes explain this error
```

Normal dictation is unchanged.

## Runtime

The adapter runs:

```bash
hermes -z "<prompt>"
```

The prompt includes:

- the spoken command after the trigger prefix
- the active app name and window title when available
- selected text when OpenTypeless selected-text capture is enabled

Hermes stdout opens in a dedicated **Agent Response** window. It is also stored
on the history item as the Agent response, so the history detail action can show
the full returned content later.

## Configuration

Open **Settings → Agent** to configure:

- whether Hermes routing is enabled
- the Hermes command path
- the working directory used when launching Hermes
- the currently configured request shape

The same pane has a **Run Hermes Test** button. A successful test returns:

```text
hermes agent test ok
```

Optional environment variables still work as fallback values when the matching setting is empty:

```bash
OPENTYPELESS_HERMES_COMMAND=/Users/rockenlee/miniconda3/bin/hermes
OPENTYPELESS_HERMES_CWD=/path/to/project
```

If the Hermes command setting and `OPENTYPELESS_HERMES_COMMAND` are both empty, OpenTypeless tries the local Hermes paths used on this machine, then falls back to `hermes` from `PATH`.

If the working directory setting and `OPENTYPELESS_HERMES_CWD` are both empty, Hermes runs from the app process current directory.

During a routed voice command, the capsule displays the Hermes command and working directory so the runtime target is visible while the agent is running.
