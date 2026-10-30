%global debug_package %{nil}
%global __strip /bin/true

Name:           nan
Version:        @VERSION@
Release:        1%{?dist}
Summary:        The NaN toolchain, built by itself
License:        Apache-2.0 WITH LLVM-exception
URL:            https://github.com/nan-language/nan
Source0:        %{url}/releases/download/v%{version}/nan-v%{version}-x86_64-linux
Source1:        %{url}/releases/download/v%{version}/nan-v%{version}-aarch64-linux
Source2:        %{url}/raw/v%{version}/LICENSE
ExclusiveArch:  x86_64 aarch64
Suggests:       gcc
Suggests:       qemu-system-x86
Suggests:       qemu-system-aarch64
Suggests:       edk2-ovmf
Suggests:       edk2-aarch64

%description
NaN is a programming language made for agentic coding. This package installs
nan, its toolchain, as built by itself.

%prep
%setup -q -c -T
cp -p %{SOURCE2} .

%install
%ifarch x86_64
install -Dm755 %{SOURCE0} %{buildroot}%{_bindir}/nan
%else
install -Dm755 %{SOURCE1} %{buildroot}%{_bindir}/nan
%endif

%files
%license LICENSE
%{_bindir}/nan

%changelog
* @DATE@ github-actions[bot] <41898282+github-actions[bot]@users.noreply.github.com> - @VERSION@-1
- Release @VERSION@
