"""CLI entrypoint for the workflow daemon: ``python -m midtown.daemon``."""

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
        "--plugin-dirs",
        required=True,
        help="Comma-separated list of plugin directories to load",
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

    plugin_dirs = [Path(d.strip()) for d in args.plugin_dirs.split(",") if d.strip()]

    daemon = WorkflowDaemon(
        socket_path=args.socket_path,
        plugin_dirs=plugin_dirs,
    )

    try:
        asyncio.run(daemon.run())
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
