// tab's passkeys in its in-app browser (see browser_logins.rs). Runs in the
// page's top frame before its scripts: `navigator.credentials` passkey calls
// go to tab, which checks the page's real address, asks for Touch ID, and
// answers with keys from its vault. Everything else goes to WebKit as usual.
(() => {
  if (window.__tabPasskeys || !window.isSecureContext || !navigator.credentials) return;
  window.__tabPasskeys = true;

  const enc = (data) => {
    const bytes = data instanceof ArrayBuffer ? new Uint8Array(data) : new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
    let s = "";
    for (const b of bytes) s += String.fromCharCode(b);
    return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  };
  const dec = (s) => {
    const b = atob(s.replace(/-/g, "+").replace(/_/g, "/") + "=".repeat((4 - (s.length % 4)) % 4));
    const out = new Uint8Array(b.length);
    for (let i = 0; i < b.length; i++) out[i] = b.charCodeAt(i);
    return out.buffer;
  };
  const refs = (list) => (list || []).map((c) => ({ id: enc(c.id) }));

  const ask = (kind, options, signal) => {
    if (signal && signal.aborted) return Promise.reject(signal.reason ?? new DOMException("The operation was aborted.", "AbortError"));
    const asked = window.__TAURI_INTERNALS__.invoke("plugin:passkey|request", { kind, options }).catch((e) => {
      throw new DOMException((e && e.message) || String(e), (e && e.name) || "NotAllowedError");
    });
    if (!signal) return asked;
    return new Promise((resolve, reject) => {
      signal.addEventListener("abort", () => reject(signal.reason ?? new DOMException("The operation was aborted.", "AbortError")), { once: true });
      asked.then(resolve, reject);
    });
  };

  const own = (proto, props) => {
    const o = Object.create(proto ? proto.prototype : Object.prototype);
    for (const [k, v] of Object.entries(props)) Object.defineProperty(o, k, { value: v, enumerable: typeof v !== "function" });
    return o;
  };

  // A PublicKeyCredential from tab's answer, which is shaped like its toJSON().
  const credential = (json, created) => {
    const r = json.response;
    const response = created
      ? own(window.AuthenticatorAttestationResponse, {
          clientDataJSON: dec(r.clientDataJSON),
          attestationObject: dec(r.attestationObject),
          getTransports: () => r.transports.slice(),
          getAuthenticatorData: () => dec(r.authenticatorData),
          getPublicKey: () => dec(r.publicKey),
          getPublicKeyAlgorithm: () => r.publicKeyAlgorithm,
        })
      : own(window.AuthenticatorAssertionResponse, {
          clientDataJSON: dec(r.clientDataJSON),
          authenticatorData: dec(r.authenticatorData),
          signature: dec(r.signature),
          userHandle: r.userHandle ? dec(r.userHandle) : null,
        });
    return own(window.PublicKeyCredential, {
      id: json.id,
      rawId: dec(json.rawId),
      type: json.type,
      authenticatorAttachment: json.authenticatorAttachment,
      response,
      getClientExtensionResults: () => ({}),
      toJSON: () => json,
    });
  };

  const container = Object.getPrototypeOf(navigator.credentials);
  const create = container.create;
  const get = container.get;

  container.create = function (options) {
    const pk = options && options.publicKey;
    if (!pk) return create.apply(this, arguments);
    return ask("create", {
      challenge: enc(pk.challenge),
      rp: { id: pk.rp && pk.rp.id, name: pk.rp && pk.rp.name },
      user: { id: enc(pk.user.id), name: pk.user.name, displayName: pk.user.displayName },
      pubKeyCredParams: pk.pubKeyCredParams || [],
      excludeCredentials: refs(pk.excludeCredentials),
    }, options.signal).then((json) => credential(json, true));
  };

  container.get = function (options) {
    const pk = options && options.publicKey;
    if (!pk) return get.apply(this, arguments);
    // Passkey autofill isn't offered, so a conditional request waits until
    // the page gives up on it.
    if (options.mediation === "conditional") {
      return new Promise((_, reject) => options.signal && options.signal.addEventListener("abort", () => reject(options.signal.reason ?? new DOMException("The operation was aborted.", "AbortError")), { once: true }));
    }
    return ask("get", {
      challenge: enc(pk.challenge),
      rpId: pk.rpId,
      allowCredentials: refs(pk.allowCredentials),
    }, options.signal).then((json) => credential(json, false));
  };

  const PKC = window.PublicKeyCredential;
  if (PKC) {
    PKC.isUserVerifyingPlatformAuthenticatorAvailable = () => Promise.resolve(true);
    PKC.isConditionalMediationAvailable = () => Promise.resolve(false);
    PKC.getClientCapabilities = () => Promise.resolve({
      conditionalCreate: false,
      conditionalGet: false,
      hybridTransport: false,
      passkeyPlatformAuthenticator: true,
      userVerifyingPlatformAuthenticator: true,
      relatedOrigins: false,
      signalAllAcceptedCredentials: false,
      signalCurrentUserDetails: false,
      signalUnknownCredential: false,
    });
  }
})();
