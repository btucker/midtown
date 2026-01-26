#!/usr/bin/env node
/**
 * Midtown Claude Code Plugin
 *
 * MCP server providing tools for lead and coworker agents to coordinate
 * via the midtown daemon. Tools shell out to the `midtown` CLI.
 */
import { Server } from "@modelcontextprotocol/sdk/server/index.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { CallToolRequestSchema, ListToolsRequestSchema, } from "@modelcontextprotocol/sdk/types.js";
import { execFile } from "child_process";
import { promisify } from "util";
const execFileAsync = promisify(execFile);
// Default to 'midtown' in PATH, but allow override
const MIDTOWN_BIN = process.env.MIDTOWN_BIN || "midtown";
/**
 * Execute a midtown CLI command and return the result.
 * Uses execFile with argument array to prevent command injection.
 */
async function runMidtown(args) {
    const fullArgs = ["--format", "json", ...args];
    try {
        const { stdout, stderr } = await execFileAsync(MIDTOWN_BIN, fullArgs);
        return { success: true, output: stdout || stderr };
    }
    catch (error) {
        const execError = error;
        return {
            success: false,
            output: execError.stderr || execError.message || "Unknown error",
        };
    }
}
// Tool definitions organized by role
const LEAD_TOOLS = [
    {
        name: "spawn_coworker",
        description: "Spawn a new coworker agent. The daemon assigns a unique name from the Manhattan avenue naming scheme.",
        inputSchema: {
            type: "object",
            properties: {},
            required: [],
        },
    },
    {
        name: "shutdown_coworker",
        description: "Gracefully shutdown a coworker agent by name. The coworker will finish current work and exit.",
        inputSchema: {
            type: "object",
            properties: {
                name: {
                    type: "string",
                    description: "Name of the coworker to shutdown (e.g., 'broadway')",
                },
            },
            required: ["name"],
        },
    },
    {
        name: "broadcast",
        description: "Post an announcement to the team channel. Use for important updates that all coworkers should see.",
        inputSchema: {
            type: "object",
            properties: {
                message: {
                    type: "string",
                    description: "The announcement message",
                },
            },
            required: ["message"],
        },
    },
];
const COWORKER_TOOLS = [
    {
        name: "post_message",
        description: "Post a message to the team channel. Also returns recent messages for context.",
        inputSchema: {
            type: "object",
            properties: {
                message: {
                    type: "string",
                    description: "The message to post",
                },
            },
            required: ["message"],
        },
    },
    {
        name: "read_channel",
        description: "Read recent messages from the team channel. Returns messages since your last read.",
        inputSchema: {
            type: "object",
            properties: {
                all: {
                    type: "boolean",
                    description: "If true, show all messages instead of just unread",
                },
            },
            required: [],
        },
    },
    {
        name: "claim_task",
        description: "Claim a task by ID. This marks you as the owner and prevents others from working on it.",
        inputSchema: {
            type: "object",
            properties: {
                task_id: {
                    type: "string",
                    description: "The task ID to claim",
                },
            },
            required: ["task_id"],
        },
    },
    {
        name: "request_review",
        description: "Request another coworker to review your PR. Posts a review request to the channel.",
        inputSchema: {
            type: "object",
            properties: {
                pr_number: {
                    type: "string",
                    description: "The pull request number or URL",
                },
                reviewer: {
                    type: "string",
                    description: "Optional: specific coworker to request review from. If not specified, any available coworker can pick it up.",
                },
                description: {
                    type: "string",
                    description: "Brief description of what to focus on in the review",
                },
            },
            required: ["pr_number"],
        },
    },
];
const SHARED_TOOLS = [
    {
        name: "list_coworkers",
        description: "List all active coworkers with their current status and assigned work.",
        inputSchema: {
            type: "object",
            properties: {},
            required: [],
        },
    },
    {
        name: "check_pr_status",
        description: "Check the status of pull requests including CI status and review state.",
        inputSchema: {
            type: "object",
            properties: {
                pr_number: {
                    type: "string",
                    description: "Optional: specific PR number to check. If not provided, lists all open PRs.",
                },
            },
            required: [],
        },
    },
];
const ALL_TOOLS = [...LEAD_TOOLS, ...COWORKER_TOOLS, ...SHARED_TOOLS];
/**
 * Handle tool execution by mapping to midtown CLI commands.
 */
async function handleToolCall(name, args) {
    let result;
    switch (name) {
        // Lead tools
        case "spawn_coworker":
            result = await runMidtown(["coworker", "spawn"]);
            break;
        case "shutdown_coworker":
            result = await runMidtown([
                "coworker",
                "shutdown",
                String(args.name),
            ]);
            break;
        case "broadcast":
            // Broadcast is a channel post with [ANNOUNCEMENT] prefix
            result = await runMidtown([
                "channel",
                "post",
                `[ANNOUNCEMENT] ${args.message}`,
            ]);
            break;
        // Coworker tools
        case "post_message":
            result = await runMidtown(["channel", "post", String(args.message)]);
            break;
        case "read_channel":
            if (args.all) {
                result = await runMidtown(["channel", "read", "--all"]);
            }
            else {
                result = await runMidtown(["channel", "read"]);
            }
            break;
        case "claim_task":
            result = await runMidtown(["task", "claim", String(args.task_id)]);
            break;
        case "request_review": {
            // Request review posts a formatted message to the channel
            const reviewer = args.reviewer ? `@${args.reviewer}` : "@team";
            const desc = args.description
                ? ` - ${args.description}`
                : "";
            const message = `[REVIEW REQUEST] ${reviewer}: Please review PR #${args.pr_number}${desc}`;
            result = await runMidtown(["channel", "post", message]);
            break;
        }
        // Shared tools
        case "list_coworkers":
            result = await runMidtown(["coworker", "list"]);
            break;
        case "check_pr_status":
            if (args.pr_number) {
                // For specific PR, we'd need pr show <id> - using list for now
                result = await runMidtown(["pr", "list"]);
            }
            else {
                result = await runMidtown(["pr", "list"]);
            }
            break;
        default:
            result = { success: false, output: `Unknown tool: ${name}` };
    }
    return {
        content: [
            {
                type: "text",
                text: result.output,
            },
        ],
    };
}
// Create and configure the MCP server
const server = new Server({
    name: "midtown",
    version: "0.1.0",
}, {
    capabilities: {
        tools: {},
    },
});
// Handle list_tools request
server.setRequestHandler(ListToolsRequestSchema, async () => {
    return { tools: ALL_TOOLS };
});
// Handle call_tool request
server.setRequestHandler(CallToolRequestSchema, async (request) => {
    const { name, arguments: args } = request.params;
    return handleToolCall(name, args ?? {});
});
// Start the server
async function main() {
    const transport = new StdioServerTransport();
    await server.connect(transport);
    console.error("Midtown MCP server started");
}
main().catch((error) => {
    console.error("Fatal error:", error);
    process.exit(1);
});
