{lib}: let
  policies = [
    "must-substitute"
    "cross-build"
    "build-platform"
  ];

  isNonEmptyString = value: builtins.isString value && value != "";

  requireString = context: field: value:
    if isNonEmptyString value
    then value
    else throw "crossbow: pin manifest ${context}.${field} must be a non-empty string";

  requireStringList = context: field: value:
    if !builtins.isList value
    then throw "crossbow: pin manifest ${context}.${field} must be a list of strings"
    else
      lib.imap0 (
        index: item:
          requireString "${context}.${field}[${toString index}]" "value" item
      )
      value;

  sortUnique = values: lib.sort builtins.lessThan (lib.unique values);

  normalizeRoot = defaultConsumerPrefix: index: rawRoot: let
    context = "root[${toString index}]";
    root =
      if builtins.isAttrs rawRoot
      then rawRoot
      else throw "crossbow: pin manifest ${context} must be an attribute set";
    channel =
      requireString context "channel"
      (
        if root ? channel
        then root.channel
        else null
      );
    inputName =
      requireString context "inputName"
      (
        if root ? inputName
        then root.inputName
        else null
      );
    name =
      requireString context "name"
      (
        if root ? name
        then root.name
        else null
      );
    policy =
      requireString context "policy"
      (
        if root ? policy
        then root.policy
        else null
      );
    target =
      requireString context "target"
      (
        if root ? target
        then root.target
        else null
      );
    archValue =
      if root ? arch
      then root.arch
      else null;
    hostSystemValue =
      if root ? hostSystem
      then root.hostSystem
      else null;
    hasArch = root ? arch && archValue != null;
    hasHostSystem = root ? hostSystem && hostSystemValue != null;
    arch =
      if hasArch && hasHostSystem && archValue != hostSystemValue
      then throw "crossbow: pin manifest ${context} has conflicting arch `${toString archValue}` and hostSystem `${toString hostSystemValue}`"
      else if hasArch
      then requireString context "arch" archValue
      else if hasHostSystem
      then requireString context "hostSystem" hostSystemValue
      else throw "crossbow: pin manifest ${context} must define `arch` or `hostSystem`";
    policyCheck =
      if builtins.elem policy policies
      then null
      else throw "crossbow: pin manifest ${context}.policy `${policy}` is invalid; expected one of ${lib.concatStringsSep ", " policies}";
    cachePackages =
      if root ? cachePackages
      then requireStringList context "cachePackages" root.cachePackages
      else [];
    packageTargets =
      if !(root ? packageTargets)
      then {}
      else if !builtins.isAttrs root.packageTargets
      then throw "crossbow: pin manifest ${context}.packageTargets must be an attribute set of package targets"
      else
        lib.mapAttrs (
          package: packageTarget:
            requireString "${context}.packageTargets" package packageTarget
        )
        root.packageTargets;
    host =
      if root ? host && root.host != null
      then requireString context "host" root.host
      else null;
    consumerPrefix =
      if root ? consumerPrefix && root.consumerPrefix != null
      then requireString context "consumerPrefix" root.consumerPrefix
      else if defaultConsumerPrefix != null
      then defaultConsumerPrefix
      else if host != null
      then "nixosConfigurations.${host}-crossbow"
      else null;
    packageTargetNames = builtins.attrNames packageTargets;
    extraPackageTargets =
      builtins.filter (package: !(builtins.elem package cachePackages))
      packageTargetNames;
    missingPackageTargets =
      builtins.filter (package: !(builtins.elem package packageTargetNames))
      cachePackages;
    packageTargetCheck =
      if cachePackages != [] && extraPackageTargets != []
      then throw "crossbow: pin manifest ${context}.packageTargets contains packages not listed in cachePackages: ${lib.concatStringsSep ", " extraPackageTargets}"
      else if cachePackages != [] && consumerPrefix == null && missingPackageTargets != []
      then throw "crossbow: pin manifest ${context}.cachePackages needs consumerPrefix or packageTargets for: ${lib.concatStringsSep ", " missingPackageTargets}"
      else null;
    packageEntries =
      if cachePackages != []
      then
        map (
          package: {
            inherit package;
            target =
              if consumerPrefix != null
              then "${consumerPrefix}.pkgs.${package}"
              else packageTargets.${package};
          }
        )
        cachePackages
      else if packageTargetNames != []
      then
        map (package: {
          inherit package;
          target = packageTargets.${package};
        })
        packageTargetNames
      else if policy == "must-substitute"
      then [
        {
          package = name;
          inherit target;
        }
      ]
      else [];
  in
    builtins.seq policyCheck (builtins.seq packageTargetCheck {
      inherit
        channel
        inputName
        arch
        name
        policy
        target
        host
        consumerPrefix
        cachePackages
        packageTargets
        packageEntries
        ;
    });

  entrySortKey = entry:
    lib.concatStringsSep "\n" [
      entry.package
      entry.target
      entry.name
      entry.policy
      entry.rootTarget
      (entry.host or "")
    ];

  sortEntries = entries:
    lib.sort (left: right: entrySortKey left < entrySortKey right) entries;

  entriesForRoot = root:
    map (
      packageEntry:
        {
          channel = root.channel;
          inputName = root.inputName;
          arch = root.arch;
          name = root.name;
          policy = root.policy;
          rootTarget = root.target;
          package = packageEntry.package;
          target = packageEntry.target;
        }
        // lib.optionalAttrs (root.host != null) {inherit (root) host;}
    )
    root.packageEntries;

  groupFor = roots: channel: let
    channelRoots = builtins.filter (root: root.channel == channel) roots;
    entries = sortEntries (lib.concatLists (map entriesForRoot channelRoots));
    packages = sortUnique (map (entry: entry.package) entries);
    architectures = sortUnique (map (root: root.arch) channelRoots);
    inputNames = sortUnique (map (root: root.inputName) channelRoots);
    targetsFor = package:
      sortUnique (map (entry: entry.target) (builtins.filter (entry: entry.package == package) entries));
    firstTargetFor = package: builtins.head (targetsFor package);
    consumerTargets = builtins.listToAttrs (map (package: {
        name = package;
        value = firstTargetFor package;
      })
      packages);
    duplicatePackages = builtins.filter (package: builtins.length (targetsFor package) > 1) packages;
      requiredConsumerTargets = builtins.listToAttrs (lib.concatLists (map (
        package:
          lib.imap0 (index: target: {
            name = "${package}#${toString (index + 1)}";
            value = target;
          })
          (builtins.tail (targetsFor package))
      )
      duplicatePackages));
  in
    if builtins.length inputNames != 1
    then throw "crossbow: pin manifest channel `${channel}` has multiple inputName values: ${lib.concatStringsSep ", " inputNames}"
    else {
      inherit channel entries packages consumerTargets requiredConsumerTargets architectures;
      inputName = builtins.head inputNames;
      arch =
        if builtins.length architectures == 1
        then builtins.head architectures
        else null;
    };
in {
  mkPinManifest = {
    roots ? [],
    consumerPrefix ? null,
  }: let
    defaultConsumerPrefix =
      if consumerPrefix == null
      then null
      else requireString "manifest" "consumerPrefix" consumerPrefix;
    rootList =
      if builtins.isList roots
      then roots
      else if builtins.isAttrs roots
      then
        map (
          name:
            roots.${name}
            // lib.optionalAttrs (!(roots.${name} ? name)) {inherit name;}
        )
        (builtins.attrNames roots)
      else throw "crossbow: pin manifest `roots` must be a list or attribute set of roots";
    normalizedRoots = lib.imap0 (normalizeRoot defaultConsumerPrefix) rootList;
    channels = sortUnique (map (root: root.channel) normalizedRoots);
    inputNameChecks =
      map (
        channel: let
          inputNames = sortUnique (map (root: root.inputName) (builtins.filter (root: root.channel == channel) normalizedRoots));
        in
          if builtins.length inputNames == 1
          then null
          else throw "crossbow: pin manifest channel `${channel}` has multiple inputName values: ${lib.concatStringsSep ", " inputNames}"
      )
      channels;
    includedRoots = builtins.filter (root: root.packageEntries != []) normalizedRoots;
    includedChannels = sortUnique (map (root: root.channel) includedRoots);
  in
    builtins.deepSeq inputNameChecks {
      schemaVersion = 1;
      groups = builtins.listToAttrs (map (channel: {
          name = channel;
          value = groupFor includedRoots channel;
        })
        includedChannels);
    };
}
