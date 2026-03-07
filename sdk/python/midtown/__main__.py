"""CLI entrypoint for the workflow daemon: ``python -m midtown``."""

from __future__ import annotations

import argparse
import asyncio
import logging
import sys
from pathlib import Path

from midtown.daemon import WorkflowDaemon


def main() -> None:
    parser = argparse.ArgumentParser(
        prog="midtown.daemon",
        description="Long-running Python workflow daemon",
    )
    parser.add_argument(
        "--socket-path",
        required=True,
        help="Path for the Unix domain socket to listen on",
    )
    parser.add_argument(
        "--workflows-dir",
        required=True,
        help="Path to the workflows directory (contains <name>/workflow.py)",
    )
    parser.add_argument(
        "--log-level",
        default="INFO",
        choices=["DEBUG", "INFO", "WARNING", "ERROR"],
        help="Logging level (default: INFO)",
    )
    args = parser.parse_args()

    logging.basicConfig(
        level=getattr(logging, args.log_level),
        format="%(asctime)s %(name)s %(levelname)s %(message)s",
        stream=sys.stderr,
    )

    daemon = WorkflowDaemon(
        socket_path=args.socket_path,
        workflows_dir=Path(args.workflows_dir),
    )

    try:
        asyncio.run(daemon.run())
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
