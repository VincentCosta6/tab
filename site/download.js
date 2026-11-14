// Points the download buttons straight at the installers on the latest
// GitHub release, so a click downloads instead of opening the release page.
// Without JavaScript, or if GitHub doesn't answer, they still go to that page.
(async () => {
  const assets = {
    mac: /_aarch64\.dmg$/,
    windows: /_x64-setup\.exe$/,
  };
  const os = /Windows/i.test(navigator.userAgent) ? "windows" : "mac";
  try {
    const res = await fetch("https://api.github.com/repos/VincentCosta6/tab/releases/latest", { headers: { Accept: "application/vnd.github+json" } });
    if (!res.ok) return;
    const release = await res.json();
    const url = (platform) => release.assets.find((a) => assets[platform].test(a.name))?.browser_download_url;
    for (const a of document.querySelectorAll("[data-download]")) {
      const platform = a.dataset.download === "auto" ? os : a.dataset.download;
      const href = url(platform);
      if (href) a.href = href;
    }
    const version = document.querySelector("[data-version]");
    if (version && release.tag_name) version.textContent = `${release.tag_name} · Free for local use · Apple silicon and Windows x64`;
  } catch {
    // Keep the links to the release page.
  }
})();
