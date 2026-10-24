"""Email plugin: give hub agents their own email addresses (Resend Inboxes).

A standalone process. It polls each configured Resend inbox, runs every new
email through a token-free gate (``policy``), delegates the ones that pass to
the mailbox's agent with the reply contract (``refocus.md`` §6), and replies
in the email thread. It touches the hub only through ordinary DMs. Design and
open product decisions: ``docs/EMAIL_PLUGIN.md``.
"""
