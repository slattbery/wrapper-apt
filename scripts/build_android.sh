archs=("armeabi-v7a" "arm64-v8a")

for arch in "${archs[@]}"; do
	cargo ndk -t $arch -- build --release
done
