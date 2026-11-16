-- pingers-server's settings. Each help text names the server.toml field it fills
-- (server.toml.in). Addresses here are deployment values: example.org stands for your own
-- domain, and nothing here ships with a real host name.
--
-- The two OHTTP gateway keys generate themselves on install (any 32 random bytes are a valid
-- X25519 key), so no key is made off the device. Rotation is yearly and two-slot: serve the
-- other slot (ohttp_served_key_id), and once no supported app build pins the old slot, Reset it
-- to make a new one (docs/deploy-nodo.md, "Rotating keys").
local function https_origin(url)
  return nodo.matches(url, "^https://[A-Za-z0-9.-]+(:[0-9]+)?$"), "must be an https:// origin with no path"
end

local function key_hex(key)
  return nodo.matches(key, "^[0-9a-f]{64}$"), "must be 64 lowercase hex characters"
end

return {
  api_version = 1,
  settings = {
    {
      name = "server_id",
      type = "url",
      label = "Public address",
      help = "The https origin this server is published at, such as https://pingers.example.org "
        .. "(server_id). It is the server's identity: RLN proofs and account registrations are "
        .. "bound to it, so changing it later makes a new server.",
      required = true,
      default = "https://pingers.$(pingers-push-relay.domain)",
      public = "server-id",
      validate = https_origin,
    },
    {
      name = "operator",
      type = "string",
      label = "Operator",
      help = "Who runs this server, as /v1/params declares it (server_operator). It must be true "
        .. "and the same as the push relay's, so the app can warn that one operator runs both. "
        .. "By default it is the push relay's operator setting.",
      required = true,
      default = "$(pingers-push-relay.operator)",
      validate = function(name)
        return nodo.matches(name, "^\\S(.{0,126}\\S)?$"), "must be 1 to 128 characters without outer spaces"
      end,
    },
    {
      name = "ohttp_relay_url",
      type = "url",
      label = "OHTTP relay",
      help = "The relay URL /v1/params advertises (ohttp_relay_urls), such as "
        .. "https://ohttp.example.org/ohttp: the address pingers-ohttp-relay's site is "
        .. "published at, followed by /ohttp.",
      required = true,
      default = "https://ohttp.$(pingers-push-relay.domain)/ohttp",
      public = "ohttp-relay-url",
      validate = function(url)
        return nodo.matches(url, "^https://[A-Za-z0-9.-]+(:[0-9]+)?/[A-Za-z0-9._~/-]*$"),
          "must be an https:// URL with a path"
      end,
    },
    {
      name = "map_host",
      type = "url",
      label = "Map host",
      help = "The map host /v1/params advertises (map_host), such as https://maps.example.org: "
        .. "the address pingers-map-host's site is published at.",
      required = true,
      default = "https://maps.$(pingers-push-relay.domain)",
      public = "map-host",
      validate = https_origin,
    },
    {
      name = "app_signing_certificate",
      type = "string",
      label = "App signing certificate",
      help = "The SHA-256 fingerprint of the certificate the Pingers app is signed with, as "
        .. "keytool prints it (app_links.certificates). /.well-known/assetlinks.json names it, "
        .. "so Android opens this server's invite links in the app directly. The default is "
        .. "the official builds' certificate; change it only for your own app builds.",
      required = true,
      default = "AA:44:E4:54:B0:F8:FA:A8:9B:8F:69:EF:72:90:6D:AA:2A:B8:C3:F2:D6:BE:87:B1:23:C2:D3:30:C9:BD:18:16",
      validate = function(fingerprint)
        return nodo.matches(fingerprint, "^([0-9A-Fa-f]{2}:){31}[0-9A-Fa-f]{2}$"),
          "must be 32 hex pairs separated by colons"
      end,
    },
    {
      name = "logging",
      type = "enum",
      label = "Logging",
      help = "off writes nothing. counts writes request totals once an hour, never an address "
        .. "or a single request (logging).",
      choices = { "off", "counts" },
      default = "off",
    },
    {
      name = "registration_code",
      type = "secret",
      label = "Registration code",
      help = "The code a device must present to make an account here (registration_codes). "
        .. "Created on install, so the server is not open to anyone who finds it. The first "
        .. "phone gets it from the QR code on the pingers app's Setup page; every invite sent "
        .. "from a phone registered with it carries it, so friends join without typing it. "
        .. "Reset it to stop new accounts from old invite links.",
      required = true,
      generate = { hex = 16 },
      -- At least 128 bits of entropy, as pingers-server requires: 32 hex digits, or 22 of the
      -- 64 letters, digits, - and _.
      validate = function(code)
        local allowed = nodo.matches(code, "^[A-Za-z0-9_-]{22,128}$")
        local hex_only = nodo.matches(code, "^[0-9A-Fa-f]+$")
        return allowed and (not hex_only or #code >= 32),
          "must be 32 to 128 hex digits, or 22 to 128 letters, digits, - or _"
      end,
    },
    {
      name = "ohttp_key_a",
      type = "secret",
      label = "Gateway key A",
      help = "OHTTP gateway private key in slot A (ohttp.keys secret). Created on install. "
        .. "Reset only when slot A is not served and no supported app build pins it.",
      required = true,
      generate = { hex = 32 },
      validate = key_hex,
    },
    {
      name = "ohttp_key_b",
      type = "secret",
      label = "Gateway key B",
      help = "OHTTP gateway private key in slot B, published in advance so app builds can pin "
        .. "it before it is served (ohttp.keys secret). Created on install.",
      required = true,
      generate = { hex = 32 },
      validate = key_hex,
    },
    {
      name = "ohttp_key_a_id",
      type = "int",
      label = "Gateway key A id",
      help = "Key id of slot A (ohttp.keys key_id). Must differ from slot B's.",
      default = 1,
      min = 1,
      max = 255,
    },
    {
      name = "ohttp_key_b_id",
      type = "int",
      label = "Gateway key B id",
      help = "Key id of slot B (ohttp.keys key_id). Must differ from slot A's.",
      default = 2,
      min = 1,
      max = 255,
    },
    {
      name = "ohttp_served_key_id",
      type = "int",
      label = "Served gateway key",
      help = "The key id GET /v1/ohttp/config serves (ohttp.served_key_id): slot A's or slot "
        .. "B's. Every configured key still opens requests, so changing it is the rotation.",
      default = 1,
      min = 1,
      max = 255,
    },
    {
      -- push_relays.public_url: the push relay's address as
      -- /v1/params advertises it; url is the loopback address the server pushes to.
      name = "push_relay_public_url",
      type = "url",
      label = "Push relay address",
      help = "The push relay's public address, such as https://push.example.org: the address "
        .. "pingers-push-relay's site is published at. Devices fetch its key config there.",
      required = true,
      default = "https://push.$(pingers-push-relay.domain)",
      public = "push-relay-public-url",
      validate = https_origin,
    },
    {
      name = "gateway_url",
      type = "url",
      label = "Gateway (relay leg)",
      help = "Where pingers-ohttp-relay on this device forwards requests. Leave it as it is.",
      default = "http://127.0.0.1:9471/ohttp",
      public = "gateway-url",
      endpoint = "tcp",
    },
    {
      name = "gateway_key_config_url",
      type = "url",
      label = "Gateway key config (relay leg)",
      help = "Where pingers-ohttp-relay on this device fetches the served key config. Leave it "
        .. "as it is.",
      default = "http://127.0.0.1:9471/v1/ohttp/config",
      public = "gateway-key-config-url",
      endpoint = "tcp",
    },
  },
  -- The push relay's key hash is learned from the relay itself (key_config_from_relay).
  retired = { "push_relay_key_config_hash" },
}
