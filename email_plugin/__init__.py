"""Email plugin: give hub agents their own email addresses (Resend Inboxes).

A standalone process. It polls each configured Resend inbox, runs every new
email through a token-free gate (``policy``), hands the ones that pass to the
mailbox's agent as a turn in the thread's hub session (``sessions``) or a
one-shot task, both under the reply contract (``refocus.md`` §6), and replies
in the email thread (or drafts, per sender). It touches the hub only through
ordinary DMs and sessions. Design and product decisions:
``docs/EMAIL_PLUGIN.md``.
"""
