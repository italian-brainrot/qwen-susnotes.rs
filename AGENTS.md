# susnotes

`susnotes` is a local note notes / knowledge base app written in pure Rust, that is being developed by you (the AI) according to the following specification:

## Features

- Notes are stored as encrypted markdown files. Name of the file is name of the note and is displayed at the top of the note, editing it edits the name. Everything is stored in a folder specified by the user.
- All notes are encrypted and protected by a password.
- A note can link to another note via #note_name or [[note name]]. When a user opens a note and it doesn't exist, it is created automatically.
- When a note opens, it is opened in view mode, where markdown is rendered. Clicking on any content enters edit mode where you can see and edit syntax-highlighted markdown source. However, if you click and drag to select something, it will remain in view mode for easy copying.
- Code blocks with syntax highlighting, and a copy button in view mode.
- Whenever an image is pasted, it should be saved to local storage. Images should display in view mode with the ability to resize them with the mouse (in view mode).
- LaTeX support.
- Support for attachments. Unsupported files open with the default application configured for that type of file. Eventually we want to directly support various formats, such as PDFs, with additional note-taking functionality.
- Clean, modern UI. Light and dark themes.
- An AI-friendly API to interact with notes programmatically.

## Journal

Journal is a special page that opens when the susnotes is opened. When you open journal, at the top it shows a note for the current day (e.g. "2026-09-25"), automatically creating it if it doesn't exist. Below it, it shows all previous non-empty daily pages. The pages are separated by a thin vertical line and the date is displayed above the line.

This may be the main page for all the content depending on how user chooses to organize their notes. For example, if you need to save important information, you might just open susnotes and quickly paste it to today's note. Next day, if you want to add additional information, you can either scroll down to past day and append it here, or paste to the current day. When you want more organization, you create new pages for specific notes by typing in a link with page name.

## Folder structure

User can specify a portable folder where all notes and configs will be saved, and can be loaded on any machine:

- assets: images and attachments
- pages: markdown files
- config: app settings

## Technology

**Pure Rust** for both frontend and backend. Dependencies should also be pure Rust whenever possible.

## References

- Logseq (`/home/qq/distrobox/ubuntu/Projects/logseq`) is the main inspiration behind susnotes. It has the same workflow with a journal page and other notes. The main difference is that Logseq is structured around notes being outlines, while I want a normal markdown editor.
- Zed (cloned `/home/qq/distrobox/ubuntu/Projects/zed`) is a code editor written entirely in Rust (both frontend and backend).

## Coding guide for AI agents

- You can edit everything except this file (AGENTS.md), this file is written by the user.
- Everything else in this repo was written by you in previous sessions, and you have full control over all code, dependencies, tooling, style, instructions, etc.
- Instructions other than this file are not necessarily authoritative (may be outdated or written by older and less capable models). You can revise them if you see fit.
- Write in your normal style, don't try to explicitly mimic existing style, this can lead to style collapse.
- You will keep developing this app in future sessions, so ensure maintainability.
- You should be very proactive with maintaining this project, if you encounter a difficulty, make sure to improve the code or the tooling so that future sessions will not encounter it. This includes improving logging, tests, development documentation, and setting up development tools. It is also completely acceptable to add and change dependencies as well do big rewrites if necessary.
- A thorough logging system should be set up so that any issues the user might have while using the app are apparent from the logs.
- Do not abuse `unsafe`.
- The app is in pre-alpha and backwards compatibility is not important.

## Development workflow

`git` and `gh` are installed and configured. Everything must be developed on a new branch and then merged via a pull request, so before making any changes, make sure you are on the correct branch. If you are starting a new task, create a new branch, then make a PR once you are done. If you are working on an existing PR, make sure you have its branch selected, and update the PR with a comment once you are done. NEVER pass commment directly in bash (which can leak commands and be destructive), use `--body-file`. After you make or update your PR, the user will trigger a code review workflow, and your PR will be accepted and merged, or you will receive a comment on what needs to be refined. Always follow this workflow unless explicilty requested otherwise by user, even for small changes.

## Code review workflow

Only when user explicitly requests code review. Thoroughly review the PR, identify if it can be merged or if it has blocking issues, and write a comment using `gh`. Use `--body-file` for comments to avoid issues with special characters. In this workflow you don't fix anything, only review the PR and write a comment.
