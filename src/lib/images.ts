// Images in chats: saving what the person pastes or drops, showing local
// files (attachments, an agent's screenshots) and opening the viewer.

import { useQuery } from "@tanstack/react-query";
import { api, inTauri } from "./api";

export const imageTypes = ["image/png", "image/jpeg", "image/gif", "image/webp"];

/** A local file path, as opposed to a URL the webview can load itself. */
export function isLocalPath(src: string) {
  return src.startsWith("/") || /^[A-Za-z]:[\\/]/.test(src) || src.startsWith("~/");
}

export const isImagePath = (s: string) => /\.(png|jpe?g|gif|webp)$/i.test(s);

/** Looks like a file (has an extension), as opposed to a folder or a command. */
export const isFilePath = (s: string) => /[^\\/]\.[A-Za-z0-9]{1,8}$/.test(s) && !/[\n<>|]/.test(s);

/** `rel` against the folder the agent works in, when it is relative and there is one. */
export function resolvePath(rel: string, base?: string | null): string {
  if (isLocalPath(rel) || /^[a-z]+:/i.test(rel) || !base || !isLocalPath(base)) return rel;
  const sep = base.includes("\\") && !base.includes("/") ? "\\" : "/";
  return base.replace(/[\\/]+$/, "") + sep + rel.replace(/^\.[\\/]/, "").replace(/[\\/]/g, sep);
}

/** Saves a pasted, dropped or picked file; returns the path the agent will read. */
export async function saveAttachment(file: File): Promise<string> {
  const name = file.name || `pasted.${file.type.split("/")[1] ?? "png"}`;
  return api.attachmentSave(name, new Uint8Array(await file.arrayBuffer()));
}

/** Something the webview can put in an `<img src>`: URLs as they are, local files read through tab. */
export function useImageSrc(src: string | null | undefined): string | undefined {
  const local = !!src && inTauri && isLocalPath(src);
  const { data } = useQuery({
    queryKey: ["image", src],
    queryFn: async () => URL.createObjectURL(new Blob([await api.imageRead(src!)])),
    enabled: local,
    staleTime: Infinity,
    gcTime: 10 * 60_000,
  });
  if (!src) return undefined;
  return local ? data : src;
}

export interface ViewerImage {
  src: string;
  name?: string;
}

/** Opens the full-size viewer on `index` of `images`. */
export function openImages(images: (string | ViewerImage)[], index = 0) {
  const list = images.map((i) => (typeof i === "string" ? { src: i } : i));
  window.dispatchEvent(new CustomEvent("tab:open-images", { detail: { images: list, index } }));
}

export const fileName = (src: string) => decodeURIComponent(src.split(/[\\/]/).pop()?.split("?")[0] ?? src);
