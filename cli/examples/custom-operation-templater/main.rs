// Copyright 2024 The Jujutsu Authors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// https://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use jj_cli::cli_util::CliRunner;
use jj_cli::operation_templater::OperationTemplateBuildFnTable;
use jj_cli::operation_templater::OperationTemplateLanguageExtension;
use jj_cli::template_parser;
use jj_cli::template_parser::TemplateParseError;
use jj_cli::templater::TemplatePropertyExt as _;
use jj_lib::extensions_map::ExtensionsMap;
use jj_lib::object_id::ObjectId as _;
use jj_lib::op_store::OperationId;
use jj_lib::operation::Operation;

struct HexCounter;

fn num_digits_in_id(id: &OperationId) -> i64 {
    let mut count = 0;
    for ch in id.hex().chars() {
        if ch.is_ascii_digit() {
            count += 1;
        }
    }
    count
}

fn num_char_in_id(operation: Operation, ch_match: char) -> i64 {
    let mut count = 0;
    for ch in operation.id().hex().chars() {
        if ch == ch_match {
            count += 1;
        }
    }
    count
}

impl OperationTemplateLanguageExtension for HexCounter {
    fn build_fn_table(&self) -> OperationTemplateBuildFnTable {
        let mut table = OperationTemplateBuildFnTable::empty();
        table.operation_methods.insert(
            "num_digits_in_id",
            |_language, _diagnostics, _build_context, property, call| {
                call.expect_no_arguments()?;
                let out_property = property.map(|operation| num_digits_in_id(operation.id()));
                Ok(out_property.into_dyn_wrapped())
            },
        );
        table.operation_methods.insert(
            "num_char_in_id",
            |_language, diagnostics, _build_context, property, call| {
                let [string_arg] = call.expect_exact_arguments()?;
                let char_arg = template_parser::catch_aliases(
                    diagnostics,
                    string_arg,
                    |_diagnostics, arg| {
                        let string = template_parser::expect_string_literal(arg)?;
                        let chars: Vec<_> = string.chars().collect();
                        match chars[..] {
                            [ch] => Ok(ch),
                            _ => Err(TemplateParseError::expression(
                                "Expected singular character argument",
                                arg.span,
                            )),
                        }
                    },
                )?;

                let out_property =
                    property.map(move |operation| num_char_in_id(operation, char_arg));
                Ok(out_property.into_dyn_wrapped())
            },
        );

        table
    }

    fn build_cache_extensions(&self, _extensions: &mut ExtensionsMap) {}
}

fn main() -> std::process::ExitCode {
    CliRunner::init()
        .add_operation_template_extension(Box::new(HexCounter))
        .run()
        .into()
}
