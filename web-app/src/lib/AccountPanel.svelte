<script lang="ts">
import type { Snippet } from "svelte";
import { onMount } from "svelte";
import { fetchAllAuthProfiles, startAuthLogin, submitAuthCode, switchAuthProfile } from "./api";
import { authProfilesByProvider, authSwitching, usageData } from "./store.ts";
import type { UsageEntry } from "./types.ts";
import { estimateTimeToFull, formatResetTime, usageColor } from "./usage-utils.ts";

interface AccountProfile {
	key: string;
	name: string;
	displayName: string;
	provider: string;
	is_current: boolean;
	has_credentials: boolean;
	usage?: UsageEntry;
}

let { footerLeft }: { footerLeft?: Snippet } = $props();
let listExpanded = $state(false);
let expandedKey = $state<string | null>(null);
let switchingKey = $state<string | null>(null);
let error = $state<string | null>(null);
let showAddHint = $state(false);
let loginKey = $state<string | null>(null);
let showCodeInput = $state(false);
let authCode = $state("");
let submittingCode = $state(false);

async function handleLogin(profileName: string, provider: string, event: MouseEvent) {
	event.stopPropagation();
	loginKey = profileName;
	error = null;
	const result = await startAuthLogin(profileName, provider);
	if (result.ok && result.url) {
		window.open(result.url, "_blank");
		showCodeInput = true;
		loginKey = null;
	} else {
		loginKey = null;
		error = result.error || "Failed to start login";
		setTimeout(() => {
			error = null;
		}, 5000);
	}
}

async function handleCodeSubmit() {
	if (!authCode.trim()) return;
	submittingCode = true;
	error = null;
	const result = await submitAuthCode(authCode.trim());
	submittingCode = false;
	showCodeInput = false;
	authCode = "";
	if (result.ok) {
		setTimeout(() => fetchAllAuthProfiles(), 1000);
	} else {
		error = result.error || "Code submission failed";
		setTimeout(() => {
			error = null;
		}, 5000);
	}
}

onMount(() => {
	fetchAllAuthProfiles();
});

// Flatten all profiles across providers, augmented with usage data
const allProfiles = $derived(
	(() => {
		const providers = Object.keys($authProfilesByProvider);

		if (providers.length === 0) {
			// No auth profiles yet — show usage-only entries as a fallback
			return $usageData.map((u) => ({
				key: `${u.provider}/${u.profile}`,
				name: u.profile,
				displayName: u.account_email || u.profile,
				provider: u.provider,
				is_current: false,
				has_credentials: true,
				usage: u,
			}));
		}

		const profiles = [];
		for (const [provider, providerProfiles] of Object.entries($authProfilesByProvider)) {
			for (const profile of providerProfiles) {
				const usage = $usageData.find((u) => u.provider === provider && u.profile === profile.name);
				profiles.push({
					key: `${provider}/${profile.name}`,
					name: profile.name,
					displayName: usage?.account_email || profile.name,
					provider,
					is_current: profile.is_current,
					has_credentials: profile.has_credentials,
					usage,
				});
			}
		}

		// Sort: current first, then by name
		return profiles.sort((a, b) => {
			if (a.is_current && !b.is_current) return -1;
			if (!a.is_current && b.is_current) return 1;
			return a.name.localeCompare(b.name);
		});
	})(),
);

const activeProfiles = $derived(allProfiles.filter((p) => p.is_current));
const inactiveProfiles = $derived(allProfiles.filter((p) => !p.is_current));
const hasMultipleProviders = $derived(Object.keys($authProfilesByProvider).length > 1);

function toggleList() {
	listExpanded = !listExpanded;
	if (!listExpanded) {
		showAddHint = false;
		expandedKey = null;
	}
}

function toggleDetail(key: string) {
	expandedKey = expandedKey === key ? null : key;
}

async function handleRowClick(profile: AccountProfile) {
	if (profile.is_current) {
		toggleDetail(profile.key);
	} else if (!profile.has_credentials) {
		toggleDetail(profile.key);
	} else if (!$authSwitching) {
		error = null;
		switchingKey = profile.key;
		const result = await switchAuthProfile(profile.name, profile.provider);
		switchingKey = null;
		if (!result.ok) {
			error = result.error || null;
			setTimeout(() => {
				error = null;
			}, 5000);
		}
	}
}
</script>

{#snippet profileRow(profile: AccountProfile)}
  {@const hasUsage = profile.usage && (profile.usage.session_resets || profile.usage.week_resets)}
  {@const isSwitching = switchingKey === profile.key}
  {@const isDetailOpen = expandedKey === profile.key}

  <button
    type="button"
    class="w-full flex items-center gap-1.5 bg-transparent border-none px-1 py-1 cursor-pointer text-left rounded transition-colors duration-100 hover:bg-sidebar-accent"
    class:text-link-default={profile.is_current}
    class:text-muted-foreground={!profile.is_current}
    class:opacity-40={!profile.has_credentials}
    class:opacity-60={isSwitching}
    class:cursor-wait={isSwitching}
    onclick={() => handleRowClick(profile)}
  >
    {#if isSwitching}
      <span class="inline-block w-2.5 h-2.5 border-[1.5px] border-muted-foreground border-t-link-default rounded-full animate-spin shrink-0"></span>
    {:else}
      <span class="text-[0.5rem] shrink-0">{profile.is_current ? '\u25CF' : '\u25CB'}</span>
    {/if}
    <span class="text-[0.65rem] truncate flex-1 min-w-0">
      {#if hasMultipleProviders}
        <span class="text-muted-foreground">{profile.provider}/</span>
      {/if}
      {profile.displayName}
    </span>
    {#if hasUsage && profile.usage}
      <span class="text-[0.65rem] tabular-nums shrink-0" style="color: {usageColor(profile.usage.session_util)}">S:{Math.round(profile.usage.session_util)}%</span>
      <span class="text-[0.65rem] tabular-nums shrink-0" style="color: {usageColor(profile.usage.week_util)}">W:{Math.round(profile.usage.week_util)}%</span>
    {:else if profile.has_credentials}
      <span class="text-[0.65rem] text-muted-foreground shrink-0">&mdash;</span>
    {:else}
      <span class="text-[0.55rem] px-1 py-px bg-destructive/20 text-destructive rounded shrink-0">login</span>
    {/if}
  </button>

  {#if isDetailOpen}
    <div class="ml-5 mb-0.5 flex flex-col gap-0.5">
      {#if hasUsage && profile.usage}
        {#each [
          { label: 'Session', util: profile.usage.session_util, resets: profile.usage.session_resets, isSession: true },
          { label: 'Week', util: profile.usage.week_util, resets: profile.usage.week_resets, isSession: false },
        ] as bar}
          {@const estimate = estimateTimeToFull(bar.util, bar.resets, bar.isSession)}
          {@const resetText = formatResetTime(bar.resets, bar.isSession)}
          <div class="flex gap-1.5 text-[0.62rem] text-muted-foreground">
            <span class="shrink-0" style="color: {usageColor(bar.util)}">{bar.label}</span>
            {#if estimate}<span>{estimate}</span>{/if}
            <span>· resets {resetText}</span>
          </div>
        {/each}
      {/if}
      <div class="flex gap-1 mt-0.5">
        <button
          type="button"
          class="text-[0.6rem] text-muted-foreground bg-sidebar-accent border border-sidebar-border rounded px-1.5 py-0.5 cursor-pointer hover:text-sidebar-foreground hover:border-link-default transition-colors self-start"
          class:opacity-60={loginKey === profile.name}
          class:cursor-wait={loginKey === profile.name}
          disabled={loginKey === profile.name}
          onclick={(e) => handleLogin(profile.name, profile.provider, e)}
        >
          {loginKey === profile.name ? 'Opening...' : 'Login'}
        </button>
        <button
          type="button"
          class="text-[0.6rem] text-muted-foreground bg-sidebar-accent border border-sidebar-border rounded px-1.5 py-0.5 cursor-pointer hover:text-sidebar-foreground hover:border-link-default transition-colors self-start"
          onclick={(e) => { e.stopPropagation(); fetchAllAuthProfiles() }}
        >
          Refresh
        </button>
      </div>
    </div>
  {/if}
{/snippet}

{#if allProfiles.length > 0}
  <div class="px-2 py-1 flex flex-col gap-0.5">
    <!-- Active profiles (always visible), click to expand usage detail -->
    {#each activeProfiles as profile}
      <div>
        {@render profileRow(profile)}
      </div>
    {/each}

    <!-- Inactive profiles (visible when expanded) -->
    {#if listExpanded}
      {#each inactiveProfiles as profile}
        <div>
          {@render profileRow(profile)}
        </div>
      {/each}

      <button
        type="button"
        class="w-full text-right text-[0.62rem] text-muted-foreground bg-transparent border-none px-1 py-0.5 cursor-pointer hover:text-sidebar-foreground transition-colors"
        onclick={() => showAddHint = !showAddHint}
      >
        + Add account
      </button>
      {#if showAddHint}
        <div class="text-[0.62rem] text-muted-foreground text-right px-1 pb-0.5">
          Run <code class="bg-sidebar-accent px-1 py-0.5 rounded text-[0.6rem]">midtown auth login &lt;email&gt;</code> in terminal
        </div>
      {/if}
    {/if}

    <!-- Bottom row: optional left content + toggle -->
    <div class="flex items-center justify-between">
      <div class="flex items-center gap-1">
        {#if footerLeft}{@render footerLeft()}{/if}
      </div>
      {#if inactiveProfiles.length > 0}
        <button
          type="button"
          class="text-[0.6rem] text-muted-foreground bg-transparent border-none px-1 py-0.5 cursor-pointer hover:text-sidebar-foreground transition-colors"
          onclick={toggleList}
        >
          {listExpanded ? 'Hide' : 'Show all'} ({inactiveProfiles.length + activeProfiles.length})
        </button>
      {/if}
    </div>

    {#if error}
      <div class="text-[0.62rem] text-destructive px-1 py-0.5">{error}</div>
    {/if}
  </div>
{/if}

{#if showCodeInput}
  <div class="fixed inset-0 z-50 bg-black/50 flex items-center justify-center p-4" onclick={() => { showCodeInput = false; authCode = ""; }}>
    <div class="bg-popover border border-border rounded-lg p-4 w-full max-w-sm shadow-lg" onclick={(e) => e.stopPropagation()}>
      <h3 class="text-sm font-medium mb-2">Paste Authorization Code</h3>
      <p class="text-xs text-muted-foreground mb-3">Complete sign-in in the browser window, copy the code from the page, and paste it here.</p>
      <input
        type="text"
        bind:value={authCode}
        placeholder="Paste code here..."
        class="w-full px-2 py-1.5 text-sm border border-border rounded bg-background focus:outline-none focus:ring-1 focus:ring-ring"
        onkeydown={(e) => { if (e.key === "Enter") handleCodeSubmit(); }}
        autofocus
      />
      <div class="flex gap-2 mt-3 justify-end">
        <button
          class="px-3 py-1 text-xs rounded border border-border hover:bg-muted"
          onclick={() => { showCodeInput = false; authCode = ""; }}
        >Cancel</button>
        <button
          class="px-3 py-1 text-xs rounded bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
          disabled={submittingCode || !authCode.trim()}
          onclick={handleCodeSubmit}
        >{submittingCode ? "Submitting..." : "Submit"}</button>
      </div>
    </div>
  </div>
{/if}
