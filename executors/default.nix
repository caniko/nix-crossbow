{lib}: {
  native-builder = {builders ? []}: {
    kind = "native-builder";
    inherit builders;
  };

  wasmtime = {wasmtime ? null}: {
    kind = "wasmtime";
    inherit wasmtime;
    phase = "declared-stub";
  };

  wine = {wine ? null}: {
    kind = "wine";
    inherit wine;
    warning = "wine executor is not hermetic and may produce false positives";
    phase = "declared-stub";
  };

  skip = {reason}: {
    kind = "skip";
    inherit reason;
  };
}
