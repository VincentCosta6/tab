/** `https://github.com/owner/repo/pull/123[/...]` → repo and number. */
export function parsePullUrl(url: string | null | undefined): { repo: string; number: number } | null {
  const m = url?.match(/github\.com\/([^/]+\/[^/]+)\/pull\/(\d+)/);
  return m ? { repo: m[1], number: Number(m[2]) } : null;
}
