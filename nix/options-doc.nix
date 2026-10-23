# Renders an options table for the docs from the module itself, so the
# option descriptions are the only copy.
#
#   nix run .#docs     rewrites the tables between the markers in place
#   nix build .#checks.<system>.docs   fails when a file is out of date
#
# `mkDoc` evaluates just one of our modules (with stubs for the host
# system's options it sets) and renders `<prefix>.*` as a markdown table.
{ lib, pkgs }:

let
  mkDoc =
    {
      module,
      prefix, # e.g. "programs.tapbridge"
      order, # option names (without prefix) in the order people read them; new ones go last
      stubs ? { },
    }:
    let
      eval = lib.evalModules {
        modules = [
          module
          {
            options = lib.mapAttrs (_: _: lib.mkOption { type = lib.types.anything; default = { }; }) stubs;
            config._module.args.pkgs = pkgs;
            config._module.check = false;
          }
        ];
      };

      all = lib.filter (o: o.visible && !o.internal && o.name != "${prefix}.package")
        (lib.optionAttrSetToDocList (lib.getAttrFromPath (lib.splitString "." prefix) eval.options));
      ordered = map (n: "${prefix}.${n}") order;
      rank = o: let i = lib.lists.findFirstIndex (n: n == o.name) null ordered; in
        if i == null then 1000 else i;
      options = lib.sort (a: b: rank a < rank b) all;

      oneLine = text: lib.concatStringsSep " "
        (lib.filter (word: word != "") (lib.splitString " "
          (lib.replaceStrings [ "\n" "|" ] [ " " "\\|" ] text)));

      short = name: lib.removePrefix "${prefix}." name;

      default = o:
        if o ? default then oneLine (o.default.text or (builtins.toJSON o.default)) else "";

      rows = map (o: "| `${short o.name}` | `${default o}` | ${oneLine (o.description or "")} |") options;
    in
    pkgs.writeText "tapbridge-options.md" (lib.concatStringsSep "\n" ([
      "| Option | Default | What it does |"
      "|---|---|---|"
    ] ++ rows) + "\n");

  stubsDarwin = {
    assertions = null;
    environment = null;
    system = null;
    launchd = null;
  };
in
{
  darwin = mkDoc {
    module = ./darwin-module.nix;
    prefix = "programs.tapbridge";
    stubs = stubsDarwin;
    order = [
      "enable" "startAtLogin" "useUsb" "alwaysPresent"
      "smartCard.allowSmartCard" "smartCard.userPairing" "smartCard.oneCardPerUser"
      "smartCard.lockOnRemoval" "smartCard.sudo" "smartCard.su" "smartCard.login"
      "smartCard.ssh.client" "smartCard.ssh.serverCardsOnly" "smartCard.enforceSmartCard"
      "smartCard.checkCertificateTrust"
    ];
  };
  nixos = mkDoc {
    module = ./nixos-module.nix;
    prefix = "services.tapbridge";
    order = [ "enable" "users" "startAtLogin" "useUsb" "virtualSecurityKey" ];
  };
  home = mkDoc {
    module = ./home-module.nix;
    prefix = "programs.tapbridge";
    order = [ "enable" "startAtLogin" "useUsb" "ifdSocket" ];
  };
}
