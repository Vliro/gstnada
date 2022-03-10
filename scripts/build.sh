SCRIPT_DIR=`pwd`
SCREAMLIB_DIR=$SCRIPT_DIR/../razor
cd $SCREAMLIB_DIR; make -j7
cd $SCRIPT_DIR
cargo build

