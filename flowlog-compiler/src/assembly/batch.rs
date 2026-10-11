//! Batch assembly. Workers publish their results to the runtime's output
//! emitters; dropping their guards joins them before the main thread emits
//! the outputs and sizes.

use flowlog_codegen::Skeleton;
use proc_macro2::TokenStream;
use quote::quote;

use crate::io::input::Input;
use crate::io::output::Output;

/// Returns the batch `main`: the runtime arguments and output setup, a
/// single dataflow run, and the outputs after the workers join. The emit
/// fragment may reference only state declared outside the workers.
pub(super) fn gen_batch_main(
    skeleton: &Skeleton,
    input: &Input,
    runtime_args: &TokenStream,
    output: &Output,
) -> TokenStream {
    let Skeleton {
        emitters,
        emitter_captures,
        worker_init,
        dataflow,
        step_loop,
        metrics_write,
        publish,
        ..
    } = skeleton;
    let Input {
        initialize_inputs,
        load_files,
        ..
    } = input;
    let Output {
        initialize: initialize_output,
        emit: emit_output,
    } = output;

    quote! {
        fn main() {
            #runtime_args
            #initialize_output

            #emitters

            let timer = Instant::now();
            timely::execute(timely_config, {
                #emitter_captures

                move |worker| {
                    let index = worker.index();

                    #worker_init

                    #dataflow

                    if index == 0 {
                        println!("{:?}:\tDataflow assembled", timer.elapsed());
                    }

                    // Closing the inputs, all static in a batch engine, is
                    // what lets the dataflow drain to fixpoint.
                    #initialize_inputs
                    #(#load_files)*
                    inputs.apply_inline_all();
                    inputs.close_static();

                    #step_loop

                    #publish

                    #metrics_write
                }
            })
            .unwrap();

            println!("{:?}:\tDataflow executed", timer.elapsed());
            #emit_output
        }
    }
}
