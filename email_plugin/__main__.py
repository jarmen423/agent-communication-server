import sys
from pathlib import Path

# The plugin imports shared repo-root modules (nats_connect, hub_delivery).
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from email_plugin.cli import main  # noqa: E402

sys.exit(main())
