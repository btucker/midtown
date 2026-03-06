# How Hard Could It Be?

## The Dangerous Question

Every bad idea I've had in the last six months started the same way: "How hard could it be?"

The answer is always harder than you think. But with AI coding agents, you don't find that out until you're already hooked. The first 80% comes so fast that you mistake momentum for progress.

This is the story of how I went from using coding agents to building a system that orchestrates them. I'm still not sure where it's going. But I've learned a few things along the way.

## Learning to Loop

Fall 2024. Cursor launches Composer in agent mode and I'm immediately in. Twenty-turn limit. Goes off the rails constantly. Half the time you'd burn ten turns just getting it back on track.

But you could build things. Prototypes, mostly. Ugly ones. The trick was staying close — reading every line, catching the hallucinations early, nudging it back before it wandered too far.

Then I started using CodeRabbit. I'd generate code with Cursor, push it, and let CodeRabbit tear it apart. Then I'd take that feedback back to Cursor for another pass.

The results got dramatically better. Not because the model improved. Because I'd added a loop.

Generate. Critique. Revise. That's the pattern. I didn't have a name for it yet, but that loop is the thing that keeps producing better results. Not better models. Better loops.

## The Models Catch Up

Early 2025. I'm spending more time in Claude Code. The harness is better — it doesn't fight you the way Cursor sometimes does. Then Opus 4.5 drops and something shifts.

You can give it a goal. A real goal, not just "fix this function." And it comes back with something reasonable. Not perfect. But the gap between what you asked for and what you got shrinks enough that you stop babysitting and start delegating.

Except now I'm the bottleneck. I've got four Claude Code windows open, each in a different worktree, each working on a different piece. I'm the router. Copy context from this window, paste it in that one. Check this PR, switch to that branch. My brain is the orchestration layer and it's not scaling.

## The Dangerous Questions

January 2025. I'm exploring alternative UIs for coding agents. The idea: what if the core metaphor isn't "code" but "concept"? Mermaid diagrams are great for representing concepts, but rendering them from the CLI is painfully slow.

"What if I re-implemented mermaid in Rust?"

How hard could it be?

A couple weeks later I've got Selkie — a working Mermaid renderer. But through building it I realize the real problem isn't visualization. It's that I'm spending all my time manually routing work between agent sessions. The tool isn't the bottleneck. I am.

Right around then, Gastown lands. It's a rush. Suddenly there's a system orchestrating sessions behind the scenes. Deacons, dogs, polecats, refineries, rigs — the concepts come fast and heavy. It works. It's also a lot.

Could there be a simpler version?

How hard could it be?

## The 80% Drug

Here's what nobody tells you about building with AI coding agents: they don't build like humans.

A human developer scaffolds first. Ugly but functional. Polish comes later, if ever. Agents do the opposite. They over-polish early. Your prototype comes back with error handling, doc comments, and beautiful formatting — for code that might not even be the right approach.

This creates an illusion. You look at the output and think: we're almost done. The code looks *finished*. But you're not almost done. You've just consumed the easy 80%. The remaining 20% — the design work, the edge cases, the integration — that's where the real time goes. And it's the part the agents can't do alone.

I fell for it hard with midtown. First couple of days, I had something running. Self-hosting, orchestrating sessions, posting to channels. I thought I was close.

Six weeks and about 1,800 PRs later, I'm still going.

## The Identity Crisis

The product kept discovering itself through use.

It started as a TUI. Pure terminal. Then I wanted to check on my agents from the couch. So a mobile PWA sprang to life. Then a desktop PWA. Then I realized: wait, agents don't actually have names. They're sessions. The UI was wrong because my mental model was wrong.

Push notifications would be nice. Oh, the channels are noisy — threads would help. Actually, the whole concept of who sees what needs rethinking.

Each week revealed what midtown should have been all along. This is what building with agents feels like. The tool changes what you think the tool should be.

## What I Actually Built

Midtown isn't an orchestrator. I mean, it orchestrates — there's a daemon that assigns tasks, spawns sessions, monitors health. But that's not the point.

The point is context.

When I'm working, ideas come fast. Before midtown, an idea meant: stop what I'm doing, open a new terminal, set up a worktree, start a new session, manually provide context. By the time I've done all that, I've lost the thread I was on.

Now I throw the idea in a channel. A coworker picks it up. I keep going with what I was doing. My brain gets to actually multi-task for the first time because the agents hold the context I'd otherwise lose.

The architecture that makes this work is a context hierarchy:

The **project lead** has the broadest context. Knows a little about everything. Coordinates across the whole system.

**Channel leads** go deeper. They're domain experts — they know the history, the active work, the open questions in their area. When I come back to a channel after a few hours, the lead remembers the last three design decisions.

**Forks** dive deep with me in threads. When I have a question that needs investigation, a fork spins up, digs in, and shares what it finds back up to the channel lead. The knowledge doesn't disappear when the conversation ends.

**Coworkers** go deepest on individual tasks. Isolated worktrees, focused context, a single goal.

Context flows up — coworker insights surface to channel leads, channel leads escalate to the project lead. And it flows down — the project lead provides cross-cutting context, channel leads give domain context to coworkers.

The hierarchy isn't about authority. It's about what each layer remembers.

## Still Building

I don't know where midtown goes.

There are days when it feels like the future of how humans work with AI. There are days when it feels like an over-engineered IRC server with delusions of grandeur.

But here's what I do know: it's never been a better time for a think-build-learn loop. Study a problem. Build something. See what breaks. Learn from the wreckage. Revise.

And with the right context architecture, you can run that loop multi-threaded. Multiple dangerous questions at once. One channel exploring a new interaction pattern while another channel is fixing the daemon while a third is writing about the whole thing.

"How hard could it be?" is still a dangerous question. I just ask it more often now.
