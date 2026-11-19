// The landing's only code: the former host answers with a permanent redirect to the same path on
// the current one, so links already sent in Teams cards keep working; every other request is the
// static site in public/.
const CURRENT = 'kuroko.elvisbrevi.cl';
const FORMER = new Set(['teams-assistant.elvisbrevi.cl']);

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    if (FORMER.has(url.hostname)) {
      url.protocol = 'https:';
      url.hostname = CURRENT;
      url.port = '';
      return Response.redirect(url.toString(), 301);
    }
    return env.ASSETS.fetch(request);
  },
};
