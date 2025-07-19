fn main() {
    prost_build::compile_protos(&["src/protobuf/subway.proto"], &["src/protobuf"]).unwrap();
}

