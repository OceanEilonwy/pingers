-- pingers-map-host's settings (map-host.toml.in). The packs themselves are uploaded with rsync;
-- docs/deploy-nodo.md, "Map packs", has the steps and the disk budget.
return {
  api_version = 1,
  settings = {
    {
      name = "max_disk",
      type = "string",
      label = "Disk budget",
      help = "The most the packs may take (max_disk), such as 40GB. pingers-tiles plan and cut "
        .. "refuse a region list over it; the host warns at start if the directory is over.",
      default = "40GB",
      validate = function(size)
        return nodo.matches(size, "^[0-9]{1,15}(B|KB|MB|GB|TB|KiB|MiB|GiB|TiB)$"),
          "must be a number and a unit, such as 40GB"
      end,
    },
    {
      -- TODO(p-relays): hosted as a space-separated string (P10).
      name = "hosted",
      type = "string",
      label = "Hosted regions",
      help = "Region id prefixes the host serves, separated by spaces (hosted), such as "
        .. "oceania/australia.",
      default = "oceania/australia",
      validate = function(list)
        return nodo.matches(list, "^[a-z0-9-]+(/[a-z0-9-]+)*( [a-z0-9-]+(/[a-z0-9-]+)*)*$"),
          "must be region ids such as oceania/australia, separated by single spaces"
      end,
    },
    {
      name = "logging",
      type = "enum",
      label = "Logging",
      help = "off writes nothing. counts writes request totals and disk use once an hour, never "
        .. "an address, a path or a region (logging).",
      choices = { "off", "counts" },
      default = "off",
    },
  },
}
