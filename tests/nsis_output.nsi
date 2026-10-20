; Compile with PAYLOAD, PLAN and OUTPUT definitions; only runs read-only validation.
Unicode true
Name "Bridge Unicode output regression"
OutFile "${OUTPUT}\nsis-output-test.exe"
RequestExecutionLevel user
SilentInstall silent
Section
 nsExec::ExecToStack '"${PAYLOAD}" setup-validate --file "${PLAN}" --nsis'
 Pop $0
 Pop $1
 FileOpen $2 "${OUTPUT}\nsis-output-result.txt" w
 FileWriteWord $2 0xFEFF
 FileWriteUTF16LE $2 "$0$\r$\n$1"
 FileClose $2
SectionEnd
