// Permanent redirect from www to the apex domain, keeping path and query.
const APEX = "agentcommunicationserver.app";

export default {
  fetch(request) {
    const url = new URL(request.url);
    url.protocol = "https:";
    url.hostname = APEX;
    url.port = "";
    return Response.redirect(url.toString(), 301);
  },
};
