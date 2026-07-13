// The parity harness's Go side: the driver that makes client-go drivable as THE ORACLE.
//
// THERE IS DELIBERATELY NO `replace`. pins.toml states the rule: "an oracle you can
// accidentally edit is not an oracle." A pseudo-version `require` is content-addressed
// via go.sum, so the oracle is deterministic by construction and verified on every
// build. The escape hatch for hacking on client-go locally is a gitignored go.work;
// CI sets GOWORK=off so it can never leak into a result, and the driver reports
// `replaced` in its `hello` so ledger-check can refuse one that did.
//
// Modelled on client-go's examples/txnkv/go.mod, NOT on its integration_tests/go.mod:
// the latter replaces client-go with a sibling path and drags in TiDB, unistore and
// PD's client for machinery we do not need. txnkv.NewClient already embeds
// *tikv.KVStore, which is all the probes require.
module github.com/getwyrd/client-rust-test/go

go 1.25.10

require github.com/pingcap/kvproto v0.0.0-20260622063236-b41e86365ce0

require (
	github.com/tikv/client-go/v2 v2.0.8-0.20260708122311-01bd8f99f4da
	github.com/tikv/pd/client v0.0.0-20260708075407-4e05b9d2c2d3
)

require (
	github.com/VividCortex/ewma v1.2.0 // indirect
	github.com/beorn7/perks v1.0.1 // indirect
	github.com/cespare/xxhash/v2 v2.3.0 // indirect
	github.com/cloudfoundry/gosigar v1.3.6 // indirect
	github.com/coreos/go-semver v0.3.1 // indirect
	github.com/coreos/go-systemd/v22 v22.5.0 // indirect
	github.com/dgryski/go-farm v0.0.0-20240924180020-3414d57e47da // indirect
	github.com/docker/go-units v0.5.0 // indirect
	github.com/gogo/protobuf v1.3.2 // indirect
	github.com/golang/protobuf v1.5.4 // indirect
	github.com/google/btree v1.1.2 // indirect
	github.com/google/uuid v1.6.0 // indirect
	github.com/grpc-ecosystem/go-grpc-middleware v1.1.0 // indirect
	github.com/munnerz/goautoneg v0.0.0-20191010083416-a7dc8b61c822 // indirect
	github.com/opentracing/opentracing-go v1.2.0 // indirect
	github.com/pingcap/errors v0.11.5-0.20241219054535-6b8c588c3122 // indirect
	github.com/pingcap/failpoint v0.0.0-20240528011301-b51a646c7c86 // indirect
	github.com/pingcap/log v1.1.1-0.20221110025148-ca232912c9f3 // indirect
	github.com/pkg/errors v0.9.1 // indirect
	github.com/prometheus/client_golang v1.20.5 // indirect
	github.com/prometheus/client_model v0.6.1 // indirect
	github.com/prometheus/common v0.55.0 // indirect
	github.com/prometheus/procfs v0.15.1 // indirect
	github.com/remyoudompheng/bigfft v0.0.0-20230129092748-24d4a6f8daec // indirect
	github.com/tiancaiamao/gp v0.0.0-20221230034425-4025bc8a4d4a // indirect
	github.com/twmb/murmur3 v1.1.3 // indirect
	go.etcd.io/etcd/api/v3 v3.5.10 // indirect
	go.etcd.io/etcd/client/pkg/v3 v3.5.10 // indirect
	go.etcd.io/etcd/client/v3 v3.5.10 // indirect
	go.uber.org/atomic v1.11.0 // indirect
	go.uber.org/multierr v1.11.0 // indirect
	go.uber.org/zap v1.26.0 // indirect
	golang.org/x/net v0.51.0 // indirect
	golang.org/x/sync v0.19.0 // indirect
	golang.org/x/sys v0.41.0 // indirect
	golang.org/x/text v0.34.0 // indirect
	google.golang.org/genproto/googleapis/api v0.0.0-20251202230838-ff82c1b0f217 // indirect
	google.golang.org/genproto/googleapis/rpc v0.0.0-20251202230838-ff82c1b0f217 // indirect
	google.golang.org/grpc v1.79.3 // indirect
	google.golang.org/protobuf v1.36.10 // indirect
	gopkg.in/natefinch/lumberjack.v2 v2.2.1 // indirect
	modernc.org/mathutil v1.7.1 // indirect
)
