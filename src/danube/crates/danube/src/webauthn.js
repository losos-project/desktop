// Web Authentication in the page's own world of the top frame, which
// WebKit leaves out of this port (webauthn.rs). It defines what the
// specification has a page see, `navigator.credentials.create`/`get` and
// `PublicKeyCredential`, and turns each call into an event that
// webauthn-bridge.js, in Danube's own world, passes on. Only a secure
// context gets it, as the specification says.
(() => {
  if (!isSecureContext || "PublicKeyCredential" in window) return;

  const REQUEST = "danube-webauthn-request";
  const ANSWER = "danube-webauthn-answer";
  // Lets only this script make the objects a page is handed.
  const MINE = Symbol();

  const toBase64url = (source) => {
    const bytes =
      source instanceof ArrayBuffer
        ? new Uint8Array(source)
        : new Uint8Array(source.buffer, source.byteOffset, source.byteLength);
    let binary = "";
    for (const b of bytes) binary += String.fromCharCode(b);
    return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  };
  const fromBase64url = (text) => {
    const binary = atob(text.replace(/-/g, "+").replace(/_/g, "/"));
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
    return bytes.buffer;
  };
  // Every buffer in the options, wherever it is, goes as base64url; the
  // signal stays here.
  const serialise = (options) =>
    JSON.parse(
      JSON.stringify(options, (key, value) =>
        key === "signal"
          ? undefined
          : value instanceof ArrayBuffer || ArrayBuffer.isView(value)
            ? toBase64url(value)
            : value,
      ),
    );

  const illegal = () => {
    throw new TypeError("Illegal constructor");
  };

  class AuthenticatorResponse {
    #clientDataJSON;
    constructor(token, clientDataJSON) {
      if (token !== MINE) illegal();
      this.#clientDataJSON = fromBase64url(clientDataJSON);
    }
    get clientDataJSON() {
      return this.#clientDataJSON;
    }
  }

  class AuthenticatorAttestationResponse extends AuthenticatorResponse {
    #r;
    constructor(token, r) {
      if (token !== MINE) illegal();
      super(token, r.clientDataJSON);
      this.#r = r;
    }
    get attestationObject() {
      return fromBase64url(this.#r.attestationObject);
    }
    getTransports() {
      return [...this.#r.transports];
    }
    getAuthenticatorData() {
      return fromBase64url(this.#r.authenticatorData);
    }
    getPublicKey() {
      return this.#r.publicKey == null ? null : fromBase64url(this.#r.publicKey);
    }
    getPublicKeyAlgorithm() {
      return this.#r.publicKeyAlgorithm;
    }
  }

  class AuthenticatorAssertionResponse extends AuthenticatorResponse {
    #r;
    constructor(token, r) {
      if (token !== MINE) illegal();
      super(token, r.clientDataJSON);
      this.#r = r;
    }
    get authenticatorData() {
      return fromBase64url(this.#r.authenticatorData);
    }
    get signature() {
      return fromBase64url(this.#r.signature);
    }
    get userHandle() {
      return this.#r.userHandle == null ? null : fromBase64url(this.#r.userHandle);
    }
  }

  class Credential {
    #id;
    constructor(token, id) {
      if (token !== MINE) illegal();
      this.#id = id;
    }
    get id() {
      return this.#id;
    }
    get type() {
      return "public-key";
    }
  }

  class PublicKeyCredential extends Credential {
    #answer;
    #response;
    constructor(token, answer) {
      if (token !== MINE) illegal();
      super(token, answer.id);
      this.#answer = answer;
      this.#response =
        "attestationObject" in answer.response
          ? new AuthenticatorAttestationResponse(MINE, answer.response)
          : new AuthenticatorAssertionResponse(MINE, answer.response);
    }
    get rawId() {
      return fromBase64url(this.#answer.id);
    }
    get response() {
      return this.#response;
    }
    get authenticatorAttachment() {
      return this.#answer.authenticatorAttachment;
    }
    getClientExtensionResults() {
      return structuredClone(this.#answer.clientExtensionResults);
    }
    // What the specification's toJSON gives: the buffers as base64url,
    // which is how they came.
    toJSON() {
      const { publicKeyAlgorithm, ...response } = this.#answer.response;
      return {
        id: this.#answer.id,
        rawId: this.#answer.id,
        type: "public-key",
        authenticatorAttachment: this.#answer.authenticatorAttachment,
        response:
          "attestationObject" in response ? { ...response, publicKeyAlgorithm } : response,
        clientExtensionResults: this.getClientExtensionResults(),
      };
    }
    // No key is built into the computer, and Danube offers no passkeys
    // in the address bar's autofill.
    static isUserVerifyingPlatformAuthenticatorAvailable() {
      return Promise.resolve(false);
    }
    static isConditionalMediationAvailable() {
      return Promise.resolve(false);
    }
    static getClientCapabilities() {
      return Promise.resolve({
        conditionalCreate: false,
        conditionalGet: false,
        hybridTransport: false,
        passkeyPlatformAuthenticator: false,
        userVerifyingPlatformAuthenticator: false,
        relatedOrigins: false,
        signalAllAcceptedCredentials: false,
        signalCurrentUserDetails: false,
        signalUnknownCredential: false,
      });
    }
    // The JSON forms some sites build their options from.
    static parseCreationOptionsFromJSON(json) {
      return {
        ...json,
        challenge: fromBase64url(json.challenge),
        user: { ...json.user, id: fromBase64url(json.user.id) },
        excludeCredentials: (json.excludeCredentials ?? []).map((c) => ({
          ...c,
          id: fromBase64url(c.id),
        })),
      };
    }
    static parseRequestOptionsFromJSON(json) {
      return {
        ...json,
        challenge: fromBase64url(json.challenge),
        allowCredentials: (json.allowCredentials ?? []).map((c) => ({
          ...c,
          id: fromBase64url(c.id),
        })),
      };
    }
  }

  let next = 1;

  // One call: the event the bridge hears, and the answer it sends back
  // with the same id. Aborting tells Danube to stop asking the key.
  const ask = (op, options) =>
    new Promise((resolve, reject) => {
      const signal = options.signal;
      if (signal?.aborted) {
        reject(signal.reason ?? new DOMException("The request was aborted.", "AbortError"));
        return;
      }
      const id = next++;
      const answered = (event) => {
        const answer = JSON.parse(event.detail);
        if (answer.id !== id) return;
        document.removeEventListener(ANSWER, answered);
        signal?.removeEventListener("abort", aborted);
        if (answer.error) {
          reject(new DOMException(answer.error.message, answer.error.name));
        } else {
          resolve(new PublicKeyCredential(MINE, answer.credential));
        }
      };
      const aborted = () => {
        document.removeEventListener(ANSWER, answered);
        document.dispatchEvent(
          new CustomEvent(REQUEST, { detail: JSON.stringify({ id, op: "cancel" }) }),
        );
        reject(signal.reason ?? new DOMException("The request was aborted.", "AbortError"));
      };
      document.addEventListener(ANSWER, answered);
      signal?.addEventListener("abort", aborted, { once: true });
      document.dispatchEvent(
        new CustomEvent(REQUEST, {
          detail: JSON.stringify({
            id,
            op,
            origin: location.origin,
            publicKey: serialise(options.publicKey),
          }),
        }),
      );
    });

  // A conditional request waits for the autofill Danube does not have:
  // it ends only when the page aborts it, and never puts up the bar.
  const never = (signal) =>
    new Promise((_, reject) =>
      signal?.addEventListener(
        "abort",
        () => reject(signal.reason ?? new DOMException("The request was aborted.", "AbortError")),
        { once: true },
      ),
    );

  class CredentialsContainer {
    constructor(token) {
      if (token !== MINE) illegal();
    }
    create(options = {}) {
      if (!options.publicKey) {
        return Promise.reject(
          new DOMException("Only public key credentials are supported.", "NotSupportedError"),
        );
      }
      return ask("create", options);
    }
    get(options = {}) {
      if (!options.publicKey) return Promise.resolve(null);
      if (options.mediation === "conditional") return never(options.signal);
      return ask("get", options);
    }
    store() {
      return Promise.reject(
        new DOMException("Only public key credentials are supported.", "NotSupportedError"),
      );
    }
    preventSilentAccess() {
      return Promise.resolve();
    }
  }

  const credentials = new CredentialsContainer(MINE);
  Object.defineProperty(Navigator.prototype, "credentials", {
    get: () => credentials,
    configurable: true,
    enumerable: true,
  });
  for (const type of [
    Credential,
    CredentialsContainer,
    PublicKeyCredential,
    AuthenticatorResponse,
    AuthenticatorAttestationResponse,
    AuthenticatorAssertionResponse,
  ]) {
    Object.defineProperty(window, type.name, {
      value: type,
      writable: true,
      configurable: true,
    });
  }
})();
