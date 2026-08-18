// Which Flueny a machine reports to.
//
// Staging and production are separate installs holding separate signal: a
// machine connected to one is invisible in the other, and a credential minted by
// one means nothing to the other. So the target is worth naming rather than
// remembering as a hostname, and worth resolving strictly, because pointing a
// client at the wrong host sends one company's derived signal to another
// company's server.
//
// Kept out of cli.ts deliberately: that file runs `main()` on import, so
// anything a test needs to read has to live somewhere importing it is free.

export const API_TARGETS: Record<string, string> = {
  staging: 'https://api.flueny.dev',
  production: 'https://api.flueny.ai',
}

/**
 * A known name, or a full http(s) URL, or null. Fails closed: a bare hostname
 * is refused rather than given a scheme, because a typo of the real host would
 * otherwise resolve to something that looks plausible and is not.
 */
export function resolveApiTarget(input: string): string | null {
  const raw = input.trim()
  const named = API_TARGETS[raw.toLowerCase()]
  if (named) return named
  if (!/^https?:\/\/[^\s/]+/.test(raw)) return null
  return raw.replace(/\/+$/, '')
}

/** `staging (https://api.flueny.dev)` for a known URL, the URL itself otherwise. */
export function describeApi(url: string): string {
  const name = Object.keys(API_TARGETS).find((key) => API_TARGETS[key] === url)
  return name ? `${name} (${url})` : url
}
