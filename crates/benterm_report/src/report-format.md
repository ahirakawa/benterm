## How to report

When you finish work that needs a report, write the report to a JSON file. The user reads the report in Benterm, from the row of this agent in the session list. Assume the reply in the terminal is not read, and keep it short.

1. Write the report to `<working directory>/{dir}/<task-name>{suffix}` with your file-writing tool (Write, or apply_patch), not with a command of the shell: a file a command wrote is not seen. Name the task with letters, digits and hyphens. If you continue the same task, overwrite the same file.
2. Keep the reply in the terminal within {reply_max} characters. Write only the conclusion and whether there is anything to decide.

The JSON has the shape below. Every key is required. Use `[]` for an array with nothing in it.

```json
{
  "title": "Name of the work",
  "goal": "What the user was trying to do with this work, in one sentence",
  "conclusion": "pass, issues or blocked",
  "summary": "What happened, or what you changed. Within {summary_max} characters. No breakdown or steps.",
  "decisions": [
    {
      "question": "What you want decided, in one sentence. Within {question_max} characters",
      "options": [
        { "label": "Name of a choice", "effect": "What happens if this is chosen" },
        { "label": "Another choice", "effect": "What happens if this is chosen" }
      ],
      "recommend": "The same string as the label of the choice you recommend",
      "reason": "Why you recommend it"
    }
  ],
  "verified": ["What you checked by actually running it"],
  "unverified": ["What you did not check, or could not check"],
  "deliverables": [
    { "path": "docs/plan.md", "summary": "What you wrote or changed, in one sentence" },
    { "path": "content/items/", "summary": "When many files are in the same folder, you may write the folder path and the count" },
    {
      "path": "src/auth/client.ts",
      "summary": "For a code file, make one item per file and write at least one place in changes",
      "changes": [
        { "line": 42, "before": "How it worked. For a new file, \"none (new file)\"", "after": "How it works now" }
      ]
    }
  ]
}
```

Follow these rules when you write it.

- conclusion is one word: `pass` when the result can be taken as it is, `issues` when it has problems that you name in summary or unverified, `blocked` when you cannot go on without help.
- Put every file you edited in this turn in deliverables.
- In goal, write what the user was trying to do with this work (the purpose), not what the work was.
- In decisions, write only what only the user can decide. Write what you decided yourself in summary or verified.
- question is shown in a row of the session list, so keep it within {question_max} characters. Put the background and the reasons in reason or in the effect of options.
- In verified, write only what you checked by actually running a command in this turn.
- When you use a name or number you gave to something during the work, say again what it means each time.
- Do not stop at the name of a field or a policy. Write what will actually be done and what will change.
- Write the report in the language the user writes in.
