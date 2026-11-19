-- pingers-map-host's settings (map-host.toml.in). The host cuts each region from the upstream
-- build the first time a phone asks for it (cache mode); docs/deploy-nodo.md, "Map packs", has
-- the region list, the upstream build and the disk budget.
return {
  api_version = 1,
  settings = {
    {
      name = "max_disk",
      type = "string",
      label = "Disk budget",
      help = "The most the packs may take (max_disk), such as 40GB. After each cut the host "
        .. "removes the least recently downloaded regions until the rest fit.",
      default = "40GB",
      validate = function(size)
        return nodo.matches(size, "^[0-9]{1,15}(B|KB|MB|GB|TB|KiB|MiB|GiB|TiB)$"),
          "must be a number and a unit, such as 40GB"
      end,
    },
    {
      name = "hosted",
      type = "string",
      label = "Hosted regions",
      help = "Region id prefixes the host serves and cuts, separated by spaces (hosted), such as "
        .. "oceania/australia. The bundled region list has Australia's states and territories "
        .. "and oceania/new-zealand.",
      default = "oceania/australia",
      validate = function(list)
        return nodo.matches(list, "^[a-z0-9-]+(/[a-z0-9-]+)*( [a-z0-9-]+(/[a-z0-9-]+)*)*$"),
          "must be region ids such as oceania/australia, separated by single spaces"
      end,
    },
    {
      name = "upstream",
      type = "url",
      label = "Upstream build",
      help = "The Protomaps planet build regions are cut from (cache.upstream). Pin a dated "
        .. "build that stays published: build.protomaps.com keeps every build for a week, and "
        .. "the last build of each basemap version for good (list: maps.protomaps.com/builds). "
        .. "Regions already cut keep their data until they are cut again.",
      default = "https://build.protomaps.com/20260811.pmtiles",
      validate = function(url)
        return nodo.matches(url, "^https?://[^ ?#]+\\.pmtiles$"),
          "must be the http(s) address of a .pmtiles file"
      end,
    },
    {
      name = "max_zoom",
      type = "string",
      label = "Zoom caps",
      help = "Optional: the deepest zoom to cut, per region prefix, as prefix=zoom pairs "
        .. "separated by spaces (max_zoom), such as oceania/australia/queensland=14. Each zoom "
        .. "less makes a pack about four times smaller. Empty cuts every region at zoom 15.",
      default = "",
      validate = function(pairs)
        return pairs == ""
            or nodo.matches(pairs,
              "^[a-z0-9-]+(/[a-z0-9-]+)*=([0-9]|1[0-5])( [a-z0-9-]+(/[a-z0-9-]+)*=([0-9]|1[0-5]))*$"),
          "must be empty or prefix=zoom pairs such as oceania/australia=14, zoom 0 to 15, "
            .. "separated by single spaces"
      end,
    },
    {
      name = "max_concurrent_extracts",
      type = "int",
      label = "Cuts at once",
      help = "How many regions may be cut at the same time (cache.max_concurrent_extracts); "
        .. "the rest wait their turn. Keep it low: each cut reads from the upstream.",
      default = 1,
      min = 1,
      max = 4,
    },
    {
      name = "logging",
      type = "enum",
      label = "Logging",
      help = "off writes nothing. counts writes request totals, cuts, evictions and disk use "
        .. "once an hour, never an address, a path or a region (logging).",
      choices = { "off", "counts" },
      default = "off",
    },
    {
      name = "gateway_url",
      type = "url",
      label = "Gateway (server leg)",
      help = "Where pingers-server on this device forwards map downloads made through the "
        .. "privacy relay, on loopback (the host's own OHTTP gateway). Leave it as it is.",
      default = "http://127.0.0.1:9474",
      public = "gateway-url",
      endpoint = "tcp",
    },
  },
}
