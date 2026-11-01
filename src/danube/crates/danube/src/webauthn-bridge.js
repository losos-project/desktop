// The bridge between webauthn.js in the page and Danube (webauthn.rs), in
// a world of Danube's own in the top frame. Only this world has the
// message handler, so a page reaches Danube through the bridge alone, and
// an embedded frame, which has no bridge, cannot reach it at all. The
// request and answer cross the two worlds as event details, which stay
// strings.
(() => {
  const REQUEST = "danube-webauthn-request";
  const ANSWER = "danube-webauthn-answer";
  const handler = window.webkit.messageHandlers.danubeWebAuthn;

  const answer = (id, body) =>
    document.dispatchEvent(
      new CustomEvent(ANSWER, { detail: JSON.stringify({ id, ...body }) }),
    );

  document.addEventListener(REQUEST, (event) => {
    if (typeof event.detail !== "string") return;
    let id;
    try {
      ({ id } = JSON.parse(event.detail));
    } catch {
      return;
    }
    handler.postMessage(event.detail).then(
      (reply) => {
        // A cancel has nothing to answer: the page has rejected already.
        if (reply === "{}") return;
        const parsed = JSON.parse(reply);
        answer(id, parsed.error ? { error: parsed.error } : { credential: parsed });
      },
      (error) => answer(id, { error: { name: "NotAllowedError", message: String(error) } }),
    );
  });
})();
