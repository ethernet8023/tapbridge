# Renders the `programs.tapbridge` options table for docs/nix-darwin.md
# from the module itself, so the option descriptions are the only copy.
#
#   nix run .#docs     rewrites the table between the markers in place
#   nix build .#checks.<system>.docs   fails when the file is out of date
{ lib, pkgs }:

let
  # Evaluate just our module, with stubs for the nix-darwin options it sets.
  eval = lib.evalModules {
    modules = [
      ./darwin-module.nix
      {
        options = {
          assertions = lib.mkOption { type = lib.types.anything; default = [ ]; };
          environment = lib.mkOption { type = lib.types.anything; default = { }; };
          system = lib.mkOption { type = lib.types.anything; default = { }; };
          launchd = lib.mkOption { type = lib.types.anything; default = { }; };
        };
        config._module.args.pkgs = pkgs;
        config._module.check = false;
      }
    ];
  };

  all = lib.filter (o: o.visible && !o.internal && o.name != "programs.tapbridge.package")
    (lib.optionAttrSetToDocList eval.options.programs.tapbridge);
  # The order people read them in: the app's General options, then the
  # smart card ones as the app lists them. Anything new goes last.
  order = map (n: "programs.tapbridge.${n}") [
    "enable" "startAtLogin" "useUsb" "alwaysPresent"
    "smartCard.allowSmartCard" "smartCard.userPairing" "smartCard.oneCardPerUser"
    "smartCard.lockOnRemoval" "smartCard.sudo" "smartCard.su" "smartCard.login"
    "smartCard.ssh.client" "smartCard.ssh.serverCardsOnly" "smartCard.enforceSmartCard"
    "smartCard.checkCertificateTrust"
  ];
  rank = o: let i = lib.lists.findFirstIndex (n: n == o.name) null order; in
    if i == null then 1000 else i;
  options = lib.sort (a: b: rank a < rank b) all;

  oneLine = text: lib.concatStringsSep " "
    (lib.filter (word: word != "") (lib.splitString " "
      (lib.replaceStrings [ "\n" "|" ] [ " " "\\|" ] text)));

  short = name: lib.removePrefix "programs.tapbridge." name;

  default = o:
    if o ? default then oneLine (o.default.text or (builtins.toJSON o.default)) else "";

  rows = map (o: "| `${short o.name}` | `${default o}` | ${oneLine (o.description or "")} |") options;
in
pkgs.writeText "tapbridge-options.md" (lib.concatStringsSep "\n" ([
  "| Option | Default | What it does |"
  "|---|---|---|"
] ++ rows) + "\n")
