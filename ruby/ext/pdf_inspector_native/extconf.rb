# frozen_string_literal: true

require "mkmf"
require "rb_sys/mkmf"

create_rust_makefile("pdf_inspector/pdf_inspector_native")
