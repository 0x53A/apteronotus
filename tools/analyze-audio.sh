#!/usr/bin/env bash
set -euo pipefail

usage() {
    echo "usage: tools/analyze-audio.sh <audio.wav> [options]"
    echo
    echo "  --start <seconds>          analysis window start (default: 0)"
    echo "  --duration <seconds>       analysis window length (default: rest of file)"
    echo "  --floor <dBFS>             silent-channel threshold (default: -90)"
    echo "  --band <low-high>          report mixed RMS for a frequency band; repeatable"
    echo "  --spectrogram <file.png>   write a spectrogram containing active channels only"
    echo "  --width <pixels>           spectrogram width (default: 1200)"
}

if [[ $# -eq 0 ]]; then
    usage >&2
    exit 2
fi
if [[ "$1" == "-h" || "$1" == "--help" ]]; then
    usage
    exit 0
fi

input=$1
shift

start=0
duration=
floor=-90
spectrogram=
width=1200
bands=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --start)
            start=${2:?--start needs a value}
            shift 2
            ;;
        --duration)
            duration=${2:?--duration needs a value}
            shift 2
            ;;
        --floor)
            floor=${2:?--floor needs a value}
            shift 2
            ;;
        --band)
            bands+=("${2:?--band needs a low-high value}")
            shift 2
            ;;
        --spectrogram)
            spectrogram=${2:?--spectrogram needs a path}
            shift 2
            ;;
        --width)
            width=${2:?--width needs a value}
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "analyze-audio: unknown option $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

if [[ ! -f "$input" ]]; then
    echo "analyze-audio: $input is not a file" >&2
    exit 1
fi
if ! command -v sox >/dev/null || ! command -v soxi >/dev/null; then
    echo "analyze-audio: SoX and soxi are required" >&2
    exit 1
fi

channels=$(soxi -c "$input")
sample_rate=$(soxi -r "$input")
file_duration=$(soxi -D "$input")
trim=(trim "$start")
window="from ${start}s to end"
if [[ -n "$duration" ]]; then
    trim+=("$duration")
    window="${start}s + ${duration}s"
fi

field() {
    local name=$1
    awk -v name="$name" '
        index($0, name) == 1 {
            value = substr($0, length(name) + 1)
            sub(/^[[:space:]]+/, "", value)
            print value
            exit
        }
    '
}

active=()
declare -a peaks
declare -a rms_values
declare -a dc_values

for ((channel = 1; channel <= channels; channel++)); do
    stats=$(sox "$input" -n "${trim[@]}" remix "$channel" stats 2>&1)
    peak=$(field "Pk lev dB" <<<"$stats")
    rms=$(field "RMS lev dB" <<<"$stats")
    dc=$(field "DC offset" <<<"$stats")
    peaks[channel]=$peak
    rms_values[channel]=$rms
    dc_values[channel]=$dc
    if awk -v rms="$rms" -v floor="$floor" \
        'BEGIN { exit !((rms != "-inf") && (rms + 0 > floor + 0)) }'
    then
        active+=("$channel")
    fi
done

echo "file	$input"
echo "sample_rate_hz	$sample_rate"
echo "duration_s	$file_duration"
echo "window	$window"
echo "channels	$channels"
echo "active_channels	${active[*]:-none}"
echo
echo "channel	peak_dbfs	rms_dbfs	dc_offset	active"
for ((channel = 1; channel <= channels; channel++)); do
    is_active=no
    for candidate in "${active[@]}"; do
        if [[ "$candidate" -eq "$channel" ]]; then
            is_active=yes
            break
        fi
    done
    echo -e "$channel\t${peaks[channel]}\t${rms_values[channel]}\t${dc_values[channel]}\t$is_active"
done

if [[ ${#active[@]} -gt 0 && ${#bands[@]} -gt 0 ]]; then
    mix=$(IFS=,; echo "${active[*]}")
    echo
    echo "band_hz	mixed_rms_dbfs"
    for band in "${bands[@]}"; do
        stats=$(sox "$input" -n "${trim[@]}" remix "$mix" sinc "$band" stats 2>&1)
        rms=$(field "RMS lev dB" <<<"$stats")
        echo -e "$band\t$rms"
    done
fi

if [[ -n "$spectrogram" ]]; then
    if [[ ${#active[@]} -eq 0 ]]; then
        echo "analyze-audio: no channel exceeds the ${floor} dBFS floor" >&2
        exit 1
    fi
    temporary=$(mktemp -d "${TMPDIR:-/tmp}/apteronotus-analysis.XXXXXX")
    trap 'rm -rf -- "$temporary"' EXIT
    active_audio="$temporary/active.wav"
    sox "$input" "$active_audio" "${trim[@]}" remix "${active[@]}"

    height=$((240 * ${#active[@]}))
    if ((height < 300)); then
        height=300
    elif ((height > 720)); then
        height=720
    fi
    title="$(basename "$input") — active channels ${active[*]}"
    sox "$active_audio" -n spectrogram \
        -x "$width" -Y "$height" -z 100 \
        -t "$title" -o "$spectrogram"
    echo
    echo "spectrogram	$spectrogram"
fi
