#!/usr/bin/env bash

set -euo pipefail

WORKDIR="/tmp/got-data"
mkdir -p "${WORKDIR}"

git-of-theseus-analyze /subject --outdir "${WORKDIR}"
git-of-theseus-stack-plot --outfile=/output/stack_plot.png "${WORKDIR}/cohorts.json"

CMD="git-of-theseus-survival-plot"

if [ "$GOT_SURVIVAL_YEARS" ]; then
  CMD="${CMD} --years=${GOT_SURVIVAL_YEARS}"
fi

if [ "$GOT_SURVIVAL_FIT" ]; then
  CMD="${CMD} --exp-fit"
fi

CMD="${CMD} --outfile=/output/survival_plot.png ${WORKDIR}/survival.json"
$CMD
