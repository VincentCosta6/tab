import { useQuery } from "@tanstack/react-query";
import { parseLink } from "./links";

/**
 * One cache entry per link, shared by every card, chip and pill that shows it
 * (a connector's backend keeps its own cache behind this). Pass `enabled:
 * false` to hold off, e.g. until a pill scrolls into view. What the preview
 * says is the connector's to decide; this only fetches and refreshes it.
 */
export function useLinkPreview(url: string | null | undefined, enabled = true) {
  const link = parseLink(url);
  const { data } = useQuery({
    queryKey: ["link-preview", link?.canonical ?? ""],
    queryFn: () => link!.preview!(),
    enabled: enabled && !!link?.preview,
    staleTime: 30_000,
    retry: false,
    // Coming back to the window refreshes anything older than staleTime, so
    // a chip doesn't keep saying "Conflicts" after the branch was fixed.
    refetchOnWindowFocus: true,
    // A connector can say its answer isn't settled (GitHub works out
    // mergeability lazily); ask again shortly so the badge ends up right.
    // Otherwise keep checking quietly (only while the window is showing)
    // until the thing is final, since a chat can stay open for hours.
    refetchInterval: (q) => {
      const d = q.state.data;
      if (d?.final) return false;
      return d?.settling ? 5_000 : d?.active ? 15_000 : 60_000;
    },
  });
  return { link, sum: data };
}
